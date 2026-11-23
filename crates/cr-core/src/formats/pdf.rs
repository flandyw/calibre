//! Text and photographs of a PDF, read without a PDF library.
//!
//! Pages are not rendered: the text is taken out of the page content in the
//! order it was written, lines are joined into paragraphs, and JPEG images
//! are kept. Objects are found by scanning the file instead of trusting its
//! cross-reference table, which also reads damaged files.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use regex::bytes::Regex;

use super::{BookText, Chapter};
use crate::error::{Error, Result};

const MAX_STREAM: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    Null,
    Number(f64),
    Name(String),
    Text(Vec<u8>),
    Array(Vec<Value>),
    Dict(Dict),
    Ref(u32),
    /// A keyword: a content operator, `true`, `obj`, `stream`...
    Word(String),
}
pub(super) type Dict = HashMap<String, Value>;

impl Value {
    fn number(&self) -> Option<f64> {
        match self {
            Value::Number(number) => Some(*number),
            _ => None,
        }
    }
    fn name(&self) -> Option<&str> {
        match self {
            Value::Name(name) => Some(name),
            _ => None,
        }
    }
}

fn is_space(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

struct Parser<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8], at: usize) -> Self {
        Self { data, at }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while let Some(byte) = self.peek() {
            if byte == b'%' {
                while self
                    .peek()
                    .is_some_and(|byte| byte != b'\n' && byte != b'\r')
                {
                    self.at += 1;
                }
            } else if is_space(byte) {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    fn word(&mut self) -> &'a [u8] {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|byte| !is_space(byte) && !is_delimiter(byte))
        {
            self.at += 1;
        }
        &self.data[start..self.at]
    }

    /// The next object or keyword; `None` at the end of the data or at a
    /// closing bracket, which is left for the caller.
    fn value(&mut self, depth: u8) -> Option<Value> {
        self.skip_space();
        let byte = self.peek()?;
        if depth > 32 {
            return None;
        }
        match byte {
            b'<' if self.data.get(self.at + 1) == Some(&b'<') => {
                self.at += 2;
                let mut dict = Dict::new();
                loop {
                    self.skip_space();
                    if self.data[self.at..].starts_with(b">>") {
                        self.at += 2;
                        break;
                    }
                    match self.value(depth + 1) {
                        Some(Value::Name(key)) => {
                            let value = self.value(depth + 1).unwrap_or(Value::Null);
                            dict.insert(key, value);
                        }
                        Some(_) => {}
                        None => {
                            // A stray delimiter must not stall the parser.
                            if self.peek().is_none() {
                                break;
                            }
                            self.at += 1;
                        }
                    }
                }
                Some(Value::Dict(dict))
            }
            b'<' => {
                self.at += 1;
                let mut bytes = Vec::new();
                let mut high = None;
                while let Some(byte) = self.peek() {
                    self.at += 1;
                    if byte == b'>' {
                        break;
                    }
                    let Some(digit) = (byte as char).to_digit(16) else {
                        continue;
                    };
                    match high.take() {
                        Some(high) => bytes.push((high << 4 | digit) as u8),
                        None => high = Some(digit),
                    }
                }
                if let Some(high) = high {
                    bytes.push((high << 4) as u8);
                }
                Some(Value::Text(bytes))
            }
            b'(' => {
                self.at += 1;
                let mut bytes = Vec::new();
                let mut open = 1;
                while let Some(byte) = self.peek() {
                    self.at += 1;
                    match byte {
                        b'(' => {
                            open += 1;
                            bytes.push(byte);
                        }
                        b')' => {
                            open -= 1;
                            if open == 0 {
                                break;
                            }
                            bytes.push(byte);
                        }
                        b'\\' => {
                            let Some(escaped) = self.peek() else { break };
                            self.at += 1;
                            match escaped {
                                b'n' => bytes.push(b'\n'),
                                b'r' => bytes.push(b'\r'),
                                b't' => bytes.push(b'\t'),
                                b'b' => bytes.push(8),
                                b'f' => bytes.push(12),
                                b'\r' => {
                                    if self.peek() == Some(b'\n') {
                                        self.at += 1;
                                    }
                                }
                                b'\n' => {}
                                b'0'..=b'7' => {
                                    let mut code = u32::from(escaped - b'0');
                                    for _ in 0..2 {
                                        match self.peek() {
                                            Some(digit @ b'0'..=b'7') => {
                                                code = code * 8 + u32::from(digit - b'0');
                                                self.at += 1;
                                            }
                                            _ => break,
                                        }
                                    }
                                    bytes.push(code as u8);
                                }
                                other => bytes.push(other),
                            }
                        }
                        other => bytes.push(other),
                    }
                }
                Some(Value::Text(bytes))
            }
            b'/' => {
                self.at += 1;
                let raw = self.word();
                let mut name = Vec::with_capacity(raw.len());
                let mut index = 0;
                while index < raw.len() {
                    let code = (raw[index] == b'#' && index + 2 < raw.len())
                        .then(|| {
                            std::str::from_utf8(&raw[index + 1..(index + 3).min(raw.len())]).ok()
                        })
                        .flatten()
                        .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                    match code {
                        Some(code) => {
                            name.push(code);
                            index += 3;
                        }
                        None => {
                            name.push(raw[index]);
                            index += 1;
                        }
                    }
                }
                Some(Value::Name(String::from_utf8_lossy(&name).into_owned()))
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.peek() {
                        None => break,
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        _ => match self.value(depth + 1) {
                            Some(item) => items.push(item),
                            None => self.at += 1,
                        },
                    }
                }
                Some(Value::Array(items))
            }
            b']' | b'>' | b')' | b'{' | b'}' => None,
            _ => {
                let word = self.word();
                if word.is_empty() {
                    self.at += 1;
                    return Some(Value::Null);
                }
                let text = std::str::from_utf8(word).unwrap_or("");
                let Ok(number) = text.parse::<f64>() else {
                    return Some(match text {
                        "null" => Value::Null,
                        _ => Value::Word(String::from_utf8_lossy(word).into_owned()),
                    });
                };
                // `12 0 R` is a reference to object 12.
                if word.iter().all(u8::is_ascii_digit) {
                    let after = self.at;
                    self.skip_space();
                    let generation = self.word();
                    if !generation.is_empty() && generation.iter().all(u8::is_ascii_digit) {
                        self.skip_space();
                        if self.word() == b"R" {
                            return Some(Value::Ref(number as u32));
                        }
                    }
                    self.at = after;
                }
                Some(Value::Number(number))
            }
        }
    }
}

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// Where an object is: in the file or in an unpacked object stream.
#[derive(Clone, Copy)]
struct Entry {
    store: Option<usize>,
    at: usize,
    /// The bytes of its stream, in the file.
    stream: Option<(usize, usize)>,
}

pub(super) struct Pdf<'a> {
    data: &'a [u8],
    objects: HashMap<u32, Entry>,
    stores: Vec<Vec<u8>>,
    catalog: Option<u32>,
    /// Page objects in file order, for files without a usable page tree.
    loose_pages: Vec<u32>,
    pub(super) info: Option<Dict>,
}

static OBJECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u)(\d{1,10})[\x00\t\n\x0c\r ]+\d{1,5}[\x00\t\n\x0c\r ]+obj\b").unwrap()
});
static TRAILER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)trailer[\x00\t\n\x0c\r ]*<<").unwrap());

impl<'a> Pdf<'a> {
    pub(super) fn parse(data: &'a [u8]) -> Result<Self> {
        if find(&data[..data.len().min(1024)], b"%PDF-", 0).is_none() {
            return Err(Error::Other("not a PDF file".into()));
        }
        let mut pdf = Pdf {
            data,
            objects: HashMap::new(),
            stores: Vec::new(),
            catalog: None,
            loose_pages: Vec::new(),
            info: None,
        };
        let mut encrypted = false;
        let mut info = None;
        let mut at = 0;
        while let Some(found) = OBJECT.captures_at(data, at) {
            let whole = found.get(0).expect("group 0 exists");
            at = whole.end();
            let Some(number) = std::str::from_utf8(&found[1])
                .ok()
                .and_then(|number| number.parse::<u32>().ok())
            else {
                continue;
            };
            let mut parser = Parser::new(data, at);
            let value = parser.value(0);
            let mut entry = Entry {
                store: None,
                at,
                stream: None,
            };
            let dict = match &value {
                Some(Value::Dict(dict)) => Some(dict),
                _ => None,
            };
            parser.skip_space();
            if let (Some(dict), true) = (dict, data[parser.at..].starts_with(b"stream")) {
                let mut start = parser.at + 6;
                if data.get(start) == Some(&b'\r') {
                    start += 1;
                }
                if data.get(start) == Some(&b'\n') {
                    start += 1;
                }
                // The stated length when it is right, else the end marker.
                let stated = dict
                    .get("Length")
                    .and_then(Value::number)
                    .map(|length| start.saturating_add(length as usize))
                    .filter(|end| {
                        let mut tail = Parser::new(data, (*end).min(data.len()));
                        tail.skip_space();
                        data[tail.at..].starts_with(b"endstream")
                    });
                let end = stated
                    .or_else(|| find(data, b"endstream", start))
                    .unwrap_or(data.len());
                entry.stream = Some((start, end));
                at = end;
            } else {
                at = parser.at.max(at);
            }
            pdf.objects.insert(number, entry);
            let Some(dict) = dict else { continue };
            match dict.get("Type").and_then(Value::name) {
                Some("Catalog") => pdf.catalog = Some(number),
                Some("Page") => pdf.loose_pages.push(number),
                Some("XRef") => {
                    encrypted |= dict.contains_key("Encrypt");
                    info = dict.get("Info").cloned().or(info);
                }
                Some("ObjStm") => {
                    let Some(unpacked) = pdf.stream(number).map(|(_, data)| data) else {
                        continue;
                    };
                    let first = dict.get("First").and_then(Value::number).unwrap_or(0.) as usize;
                    let count = dict.get("N").and_then(Value::number).unwrap_or(0.) as usize;
                    let store = pdf.stores.len();
                    let mut header = Parser::new(&unpacked, 0);
                    for _ in 0..count.min(100_000) {
                        let (Some(Value::Number(inner)), Some(Value::Number(offset))) =
                            (header.value(0), header.value(0))
                        else {
                            break;
                        };
                        let at = first.saturating_add(offset as usize);
                        if at >= unpacked.len() {
                            continue;
                        }
                        let inner = inner as u32;
                        pdf.objects.insert(
                            inner,
                            Entry {
                                store: Some(store),
                                at,
                                stream: None,
                            },
                        );
                        if let Some(Value::Dict(dict)) = Parser::new(&unpacked, at).value(0) {
                            match dict.get("Type").and_then(Value::name) {
                                Some("Catalog") => pdf.catalog = Some(inner),
                                Some("Page") => pdf.loose_pages.push(inner),
                                _ => {}
                            }
                        }
                    }
                    pdf.stores.push(unpacked);
                }
                _ => {}
            }
        }
        for found in TRAILER.find_iter(data) {
            if let Some(Value::Dict(trailer)) = Parser::new(data, found.end() - 2).value(0) {
                encrypted |= trailer.contains_key("Encrypt");
                info = trailer.get("Info").cloned().or(info);
            }
        }
        if encrypted {
            return Err(Error::Other(
                "this PDF is encrypted; its text cannot be read".into(),
            ));
        }
        pdf.info = info.and_then(|info| match pdf.resolve(&info) {
            Value::Dict(info) => Some(info),
            _ => None,
        });
        Ok(pdf)
    }

    pub(super) fn object(&self, number: u32) -> Option<Value> {
        let entry = self.objects.get(&number)?;
        let data = match entry.store {
            Some(store) => self.stores.get(store)?.as_slice(),
            None => self.data,
        };
        Parser::new(data, entry.at).value(0)
    }

    /// Follow references to the object they name.
    pub(super) fn resolve(&self, value: &Value) -> Value {
        let mut value = value.clone();
        for _ in 0..16 {
            match value {
                Value::Ref(number) => value = self.object(number).unwrap_or(Value::Null),
                other => return other,
            }
        }
        Value::Null
    }

    fn get(&self, dict: &Dict, key: &str) -> Value {
        dict.get(key)
            .map_or(Value::Null, |value| self.resolve(value))
    }

    fn dict(&self, dict: &Dict, key: &str) -> Option<Dict> {
        match self.get(dict, key) {
            Value::Dict(dict) => Some(dict),
            _ => None,
        }
    }

    /// The names of a stream's filters, outermost first.
    fn filters(&self, dict: &Dict) -> Vec<String> {
        match self.get(dict, "Filter") {
            Value::Name(name) => vec![name],
            Value::Array(names) => names
                .iter()
                .filter_map(|name| self.resolve(name).name().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn raw_stream(&self, number: u32) -> Option<(Dict, &'a [u8])> {
        let (start, end) = self.objects.get(&number)?.stream?;
        match self.object(number)? {
            Value::Dict(dict) => Some((dict, self.data.get(start..end)?)),
            _ => None,
        }
    }

    /// A stream with its filters undone. `None` when it uses a filter that
    /// is not supported (images, mostly).
    fn stream(&self, number: u32) -> Option<(Dict, Vec<u8>)> {
        let (dict, raw) = self.raw_stream(number)?;
        let parameters = match self.get(&dict, "DecodeParms") {
            Value::Dict(one) => vec![Some(one)],
            Value::Array(many) => many
                .iter()
                .map(|item| match self.resolve(item) {
                    Value::Dict(dict) => Some(dict),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut data = raw.to_vec();
        for (index, filter) in self.filters(&dict).iter().enumerate() {
            data = match filter.as_str() {
                "FlateDecode" | "Fl" => {
                    let mut inflated = Vec::new();
                    let complete = flate2::read::ZlibDecoder::new(data.as_slice())
                        .take(MAX_STREAM)
                        .read_to_end(&mut inflated)
                        .is_ok();
                    // A cut-off stream still has its beginning.
                    if !complete && inflated.is_empty() {
                        return None;
                    }
                    match parameters.get(index).and_then(Option::as_ref) {
                        Some(parameters) => unpredict(inflated, parameters),
                        None => inflated,
                    }
                }
                "ASCIIHexDecode" | "AHx" => {
                    let mut text = vec![b'<'];
                    text.extend_from_slice(&data);
                    match Parser::new(&text, 0).value(0) {
                        Some(Value::Text(bytes)) => bytes,
                        _ => return None,
                    }
                }
                "ASCII85Decode" | "A85" => ascii85(&data),
                _ => return None,
            };
        }
        Some((dict, data))
    }
}

impl Pdf<'_> {
    /// A losslessly stored grey or RGB image, as a PNG file.
    fn png(&self, number: u32) -> Option<Vec<u8>> {
        let (dict, _) = self.raw_stream(number)?;
        let side = |key: &str| self.get(&dict, key).number().unwrap_or(0.) as u32;
        let (width, height) = (side("Width"), side("Height"));
        let flate =
            matches!(self.filters(&dict).as_slice(), [one] if one == "FlateDecode" || one == "Fl");
        if !flate
            || side("BitsPerComponent") != 8
            || u64::from(width) * u64::from(height) > 64_000_000
        {
            return None;
        }
        let channels = match self.get(&dict, "ColorSpace") {
            Value::Name(name) => match name.as_str() {
                "DeviceRGB" | "CalRGB" => 3,
                "DeviceGray" | "CalGray" => 1,
                _ => return None,
            },
            Value::Array(parts) if parts.first().and_then(Value::name) == Some("ICCBased") => {
                let Some(Value::Ref(profile)) = parts.get(1) else {
                    return None;
                };
                match self
                    .raw_stream(*profile)?
                    .0
                    .get("N")
                    .and_then(Value::number)
                {
                    Some(3.) => 3,
                    Some(1.) => 1,
                    _ => return None,
                }
            }
            _ => return None,
        };
        let (_, pixels) = self.stream(number)?;
        if pixels.len() != width as usize * height as usize * channels {
            return None;
        }
        let color = if channels == 3 {
            image::ExtendedColorType::Rgb8
        } else {
            image::ExtendedColorType::L8
        };
        let mut png = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut png),
            &pixels,
            width,
            height,
            color,
        )
        .ok()?;
        Some(png)
    }
}

/// Undo the PNG row filters of a predicted stream.
fn unpredict(data: Vec<u8>, parameters: &Dict) -> Vec<u8> {
    let number = |key: &str, default: usize| {
        parameters
            .get(key)
            .and_then(Value::number)
            .map_or(default, |value| value as usize)
    };
    if number("Predictor", 1) < 10 {
        return data;
    }
    let bits = number("Colors", 1) * number("BitsPerComponent", 8);
    let pixel = (bits / 8).max(1);
    let row = (number("Columns", 1) * bits).div_ceil(8);
    if row == 0 {
        return data;
    }
    let mut output: Vec<u8> = Vec::with_capacity(data.len());
    let mut previous = vec![0u8; row];
    for line in data.chunks(row + 1) {
        let (kind, line) = (line[0], &line[1..]);
        let mut current = vec![0u8; row];
        for index in 0..line.len() {
            let left = if index >= pixel {
                current[index - pixel]
            } else {
                0
            };
            let up = previous[index];
            let corner = if index >= pixel {
                previous[index - pixel]
            } else {
                0
            };
            let predicted = match kind {
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => {
                    let estimate = i16::from(left) + i16::from(up) - i16::from(corner);
                    let distance = |value: u8| (estimate - i16::from(value)).abs();
                    if distance(left) <= distance(up) && distance(left) <= distance(corner) {
                        left
                    } else if distance(up) <= distance(corner) {
                        up
                    } else {
                        corner
                    }
                }
                _ => 0,
            };
            current[index] = line[index].wrapping_add(predicted);
        }
        output.extend_from_slice(&current[..line.len()]);
        previous = current;
    }
    output
}

fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut group = Vec::with_capacity(5);
    for &byte in data {
        match byte {
            b'~' => break,
            b'z' if group.is_empty() => output.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group.push(u32::from(byte - b'!'));
                if group.len() == 5 {
                    let value = group.iter().fold(0u32, |value, digit| {
                        value.wrapping_mul(85).wrapping_add(*digit)
                    });
                    output.extend_from_slice(&value.to_be_bytes());
                    group.clear();
                }
            }
            _ => {}
        }
    }
    if group.len() > 1 {
        let kept = group.len() - 1;
        group.resize(5, 84);
        let value = group.iter().fold(0u32, |value, digit| {
            value.wrapping_mul(85).wrapping_add(*digit)
        });
        output.extend_from_slice(&value.to_be_bytes()[..kept]);
    }
    output
}

// ---------------------------------------------------------------------------
// Fonts: from character codes to text
// ---------------------------------------------------------------------------

/// Where StandardEncoding differs from Windows-1252.
const STANDARD: [(u8, char); 22] = [
    (0x27, '\u{2019}'),
    (0x60, '\u{2018}'),
    (0xA4, '\u{2044}'),
    (0xA6, '\u{192}'),
    (0xA8, '\u{a4}'),
    (0xA9, '\''),
    (0xAA, '\u{201c}'),
    (0xAC, '\u{2039}'),
    (0xAD, '\u{203a}'),
    (0xAE, '\u{fb01}'),
    (0xAF, '\u{fb02}'),
    (0xB1, '\u{2013}'),
    (0xB2, '\u{2020}'),
    (0xB3, '\u{2021}'),
    (0xB7, '\u{2022}'),
    (0xB9, '\u{201a}'),
    (0xBA, '\u{201d}'),
    (0xBB, '\u{bb}'),
    (0xBC, '\u{2026}'),
    (0xD0, '\u{2014}'),
    (0xF5, '\u{131}'),
    (0xFA, '\u{153}'),
];

const GLYPHS: [(&str, char); 64] = [
    ("space", ' '),
    ("exclam", '!'),
    ("quotedbl", '"'),
    ("numbersign", '#'),
    ("dollar", '$'),
    ("percent", '%'),
    ("ampersand", '&'),
    ("quotesingle", '\''),
    ("parenleft", '('),
    ("parenright", ')'),
    ("asterisk", '*'),
    ("plus", '+'),
    ("comma", ','),
    ("hyphen", '-'),
    ("minus", '\u{2212}'),
    ("period", '.'),
    ("slash", '/'),
    ("colon", ':'),
    ("semicolon", ';'),
    ("less", '<'),
    ("equal", '='),
    ("greater", '>'),
    ("question", '?'),
    ("at", '@'),
    ("bracketleft", '['),
    ("backslash", '\\'),
    ("bracketright", ']'),
    ("underscore", '_'),
    ("braceleft", '{'),
    ("bar", '|'),
    ("braceright", '}'),
    ("quoteleft", '\u{2018}'),
    ("quoteright", '\u{2019}'),
    ("quotedblleft", '\u{201c}'),
    ("quotedblright", '\u{201d}'),
    ("quotesinglbase", '\u{201a}'),
    ("quotedblbase", '\u{201e}'),
    ("guillemotleft", '\u{ab}'),
    ("guillemotright", '\u{bb}'),
    ("endash", '\u{2013}'),
    ("emdash", '\u{2014}'),
    ("bullet", '\u{2022}'),
    ("ellipsis", '\u{2026}'),
    ("dagger", '\u{2020}'),
    ("degree", '\u{b0}'),
    ("copyright", '\u{a9}'),
    ("registered", '\u{ae}'),
    ("trademark", '\u{2122}'),
    ("section", '\u{a7}'),
    ("paragraph", '\u{b6}'),
    ("germandbls", '\u{df}'),
    ("ae", '\u{e6}'),
    ("AE", '\u{c6}'),
    ("oe", '\u{153}'),
    ("OE", '\u{152}'),
    ("oslash", '\u{f8}'),
    ("Oslash", '\u{d8}'),
    ("dotlessi", '\u{131}'),
    ("fi", '\u{fb01}'),
    ("fl", '\u{fb02}'),
    ("ff", '\u{fb00}'),
    ("ffi", '\u{fb03}'),
    ("ffl", '\u{fb04}'),
    ("nbspace", '\u{a0}'),
];
const DIGITS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];
/// Accent names with the letters that take them and the accented forms.
const ACCENTS: [(&str, &str, &str); 8] = [
    ("acute", "aeiouyAEIOUYcnszCNSZ", "áéíóúýÁÉÍÓÚÝćńśźĆŃŚŹ"),
    ("grave", "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
    ("circumflex", "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
    ("dieresis", "aeiouyAEIOUY", "äëïöüÿÄËÏÖÜŸ"),
    ("tilde", "anoANO", "ãñõÃÑÕ"),
    ("cedilla", "cCsS", "çÇşŞ"),
    ("ring", "aAuU", "åÅůŮ"),
    ("caron", "cdenrstzCDENRSTZ", "čďěňřšťžČĎĚŇŘŠŤŽ"),
];

fn glyph(name: &str) -> Option<char> {
    let name = name.split('.').next().unwrap_or(name);
    let mut letters = name.chars();
    if let (Some(letter), None) = (letters.next(), letters.next()) {
        return Some(letter);
    }
    if let Some(found) = GLYPHS.iter().find(|(known, _)| *known == name) {
        return Some(found.1);
    }
    if let Some(digit) = DIGITS.iter().position(|digit| *digit == name) {
        return char::from_digit(digit as u32, 10);
    }
    let hex = name.strip_prefix("uni").or_else(|| name.strip_prefix('u'));
    if let Some(code) =
        hex.and_then(|hex| u32::from_str_radix(hex.get(..4.min(hex.len()))?, 16).ok())
    {
        return char::from_u32(code);
    }
    let letter = name.chars().next()?;
    let (_, bases, accented) = ACCENTS
        .iter()
        .find(|accent| accent.0 == &name[letter.len_utf8()..])?;
    accented
        .chars()
        .nth(bases.chars().position(|base| base == letter)?)
}

#[derive(Default)]
struct Font {
    /// Bytes per character code.
    width: usize,
    unicode: HashMap<u32, String>,
    /// For one-byte codes without a `unicode` entry; empty for composite fonts.
    table: Vec<char>,
    widths: HashMap<u32, f64>,
    default_width: f64,
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn code_of(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .take(4)
        .fold(0, |code, byte| code << 8 | u32::from(*byte))
}

impl Font {
    fn load(pdf: &Pdf, dict: &Dict) -> Self {
        let composite = dict.get("Subtype").and_then(Value::name) == Some("Type0");
        let mut font = Font {
            width: if composite { 2 } else { 1 },
            default_width: if composite { 1000. } else { 500. },
            ..Font::default()
        };
        if let Some(Value::Ref(number)) = dict.get("ToUnicode") {
            if let Some((_, cmap)) = pdf.stream(*number) {
                font.read_cmap(&cmap);
            }
        }
        if composite {
            let descendant = match pdf.get(dict, "DescendantFonts") {
                Value::Array(fonts) => fonts.first().map(|font| pdf.resolve(font)),
                _ => None,
            };
            if let Some(Value::Dict(descendant)) = descendant {
                if let Some(width) = pdf.get(&descendant, "DW").number() {
                    font.default_width = width;
                }
                // `c [w1 w2 ...]` or `first last w`.
                if let Value::Array(items) = pdf.get(&descendant, "W") {
                    let mut items = items.iter().map(|item| pdf.resolve(item));
                    while let Some(Value::Number(first)) = items.next() {
                        match items.next() {
                            Some(Value::Array(widths)) => {
                                for (offset, width) in widths.iter().enumerate() {
                                    if let Some(width) = width.number() {
                                        font.widths.insert(
                                            (first as u32).wrapping_add(offset as u32),
                                            width,
                                        );
                                    }
                                }
                            }
                            Some(Value::Number(last)) => {
                                let Some(Value::Number(width)) = items.next() else {
                                    break;
                                };
                                for code in first as u32
                                    ..=(last as u32).min((first as u32).saturating_add(65535))
                                {
                                    font.widths.insert(code, width);
                                }
                            }
                            _ => break,
                        }
                    }
                }
            }
            return font;
        }
        let encoding = pdf.get(dict, "Encoding");
        let base = match &encoding {
            Value::Name(name) => Some(name.clone()),
            Value::Dict(encoding) => encoding
                .get("BaseEncoding")
                .and_then(Value::name)
                .map(str::to_string),
            _ => None,
        };
        let codes: Vec<u8> = (0..=255).collect();
        let decoder = match base.as_deref() {
            Some("MacRomanEncoding") => encoding_rs::MACINTOSH,
            _ => encoding_rs::WINDOWS_1252,
        };
        font.table = decoder
            .decode_without_bom_handling(&codes)
            .0
            .chars()
            .collect();
        font.table.resize(256, '\u{fffd}');
        if base.is_none() {
            // TeX fonts, which state no encoding, have ligatures here.
            for (code, ligature) in "\u{fb00}\u{fb01}\u{fb02}\u{fb03}\u{fb04}"
                .chars()
                .enumerate()
            {
                font.table[0x0B + code] = ligature;
            }
        }
        if matches!(base.as_deref(), Some("StandardEncoding")) {
            for (code, character) in STANDARD {
                font.table[usize::from(code)] = character;
            }
        }
        if let Value::Dict(encoding) = &encoding {
            if let Value::Array(differences) = pdf.get(encoding, "Differences") {
                let mut code = 0usize;
                for item in &differences {
                    match item {
                        Value::Number(start) => code = *start as usize,
                        Value::Name(name) => {
                            if let (Some(slot), Some(character)) =
                                (font.table.get_mut(code), glyph(name))
                            {
                                *slot = character;
                            }
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
        let first = pdf.get(dict, "FirstChar").number().unwrap_or(0.) as u32;
        if let Value::Array(widths) = pdf.get(dict, "Widths") {
            for (offset, width) in widths.iter().enumerate() {
                if let Some(width) = pdf.resolve(width).number() {
                    font.widths.insert(first.wrapping_add(offset as u32), width);
                }
            }
        }
        font
    }

    fn read_cmap(&mut self, data: &[u8]) {
        let mut parser = Parser::new(data, 0);
        let mut operands: Vec<Value> = Vec::new();
        let mut total = 0usize;
        while parser.at < data.len() {
            let Some(value) = parser.value(0) else {
                parser.at += 1;
                continue;
            };
            let Value::Word(word) = value else {
                operands.push(value);
                continue;
            };
            match word.as_str() {
                "endcodespacerange" => {
                    if let Some(Value::Text(low)) = operands.first() {
                        self.width = low.len().clamp(1, 4);
                    }
                }
                "endbfchar" => {
                    for pair in operands.chunks(2) {
                        if let [Value::Text(code), Value::Text(text)] = pair {
                            self.unicode.insert(code_of(code), utf16(text));
                        }
                    }
                }
                "endbfrange" => {
                    for triple in operands.chunks(3) {
                        let [Value::Text(low), Value::Text(high), target] = triple else {
                            continue;
                        };
                        let (low, high) = (code_of(low), code_of(high));
                        for code in low..=high.min(low.saturating_add(65535)) {
                            total += 1;
                            if total > 200_000 {
                                return;
                            }
                            let text = match target {
                                Value::Text(start) => {
                                    // The last unit counts up through the range.
                                    let mut units: Vec<u16> = start
                                        .chunks(2)
                                        .map(|pair| {
                                            u16::from_be_bytes([
                                                pair[0],
                                                *pair.get(1).unwrap_or(&0),
                                            ])
                                        })
                                        .collect();
                                    if let Some(last) = units.last_mut() {
                                        *last = last.wrapping_add((code - low) as u16);
                                    }
                                    String::from_utf16_lossy(&units)
                                }
                                Value::Array(each) => match each.get((code - low) as usize) {
                                    Some(Value::Text(text)) => utf16(text),
                                    _ => continue,
                                },
                                _ => continue,
                            };
                            self.unicode.insert(code, text);
                        }
                    }
                }
                _ => {}
            }
            operands.clear();
        }
    }

    /// The text of a string and how far it moves the pen, in thousandths
    /// of the font size.
    fn decode(&self, bytes: &[u8], out: &mut String) -> f64 {
        let mut advance = 0.;
        for code in bytes.chunks(self.width.max(1)) {
            let number = code_of(code);
            advance += self
                .widths
                .get(&number)
                .copied()
                .unwrap_or(self.default_width);
            if let Some(text) = self.unicode.get(&number) {
                out.push_str(text);
            } else if let Some(character) = self.table.get(number as usize) {
                if !character.is_control() {
                    out.push(*character);
                }
            }
        }
        advance
    }
}

// ---------------------------------------------------------------------------
// Page content
// ---------------------------------------------------------------------------

type Matrix = [f64; 6];
const IDENTITY: Matrix = [1., 0., 0., 1., 0., 0.];

/// `first` applied before `second`.
fn concat(first: &Matrix, second: &Matrix) -> Matrix {
    [
        first[0] * second[0] + first[1] * second[2],
        first[0] * second[1] + first[1] * second[3],
        first[2] * second[0] + first[3] * second[2],
        first[2] * second[1] + first[3] * second[3],
        first[4] * second[0] + first[5] * second[2] + second[4],
        first[4] * second[1] + first[5] * second[3] + second[5],
    ]
}

/// A line of text as it was placed on the page.
#[derive(Debug, Clone)]
struct Line {
    text: String,
    x: f64,
    y: f64,
    size: f64,
}

enum Item {
    Line(Line),
    /// A JPEG file.
    Image(Vec<u8>),
}

struct Reader<'p, 'a> {
    pdf: &'p Pdf<'a>,
    fonts: HashMap<u32, Font>,
    /// Images already placed, so that a logo on every page shows once.
    seen: HashSet<u32>,
    items: Vec<Item>,
    line: Option<Line>,
    /// Where the pen is after the last text shown, and the size it had.
    pen: (f64, f64),
}

impl Reader<'_, '_> {
    fn end_line(&mut self) {
        if let Some(line) = self.line.take() {
            if !line.text.trim().is_empty() {
                self.items.push(Item::Line(line));
            }
        }
    }

    fn show(&mut self, text: &str, x: f64, y: f64, size: f64) {
        if text.is_empty() {
            return;
        }
        let same_line = self.line.as_ref().is_some_and(|line| {
            (line.y - y).abs() < 0.4 * size.max(line.size) && x + size > self.pen.0 - size
        });
        if !same_line {
            self.end_line();
            self.line = Some(Line {
                text: String::new(),
                x,
                y,
                size,
            });
        }
        let line = self.line.as_mut().expect("a line was just opened");
        if same_line
            && x - self.pen.0 > 0.2 * size
            && !line.text.ends_with(' ')
            && !text.starts_with(' ')
        {
            line.text.push(' ');
        }
        line.text.push_str(text);
        line.size = line.size.max(size);
    }

    fn content(&mut self, data: &[u8], resources: &Dict, base: Matrix, depth: u8) {
        let pdf = self.pdf;
        let font_refs = pdf.dict(resources, "Font").unwrap_or_default();
        let objects = pdf.dict(resources, "XObject").unwrap_or_default();
        let mut parser = Parser::new(data, 0);
        let mut operands: Vec<Value> = Vec::new();
        let mut ctm = base;
        let mut saved: Vec<Matrix> = Vec::new();
        let (mut text_matrix, mut line_matrix) = (IDENTITY, IDENTITY);
        let mut font: Option<u32> = None;
        let (mut size, mut leading, mut spacing, mut word_spacing) = (0f64, 0f64, 0f64, 0f64);
        let inline = Font {
            width: 1,
            table: (0u8..=255).map(char::from).collect(),
            default_width: 500.,
            ..Font::default()
        };
        while parser.at < data.len() {
            let Some(value) = parser.value(0) else {
                parser.at += 1;
                continue;
            };
            let Value::Word(operator) = value else {
                if operands.len() < 64 {
                    operands.push(value);
                }
                continue;
            };
            let numbers: Vec<f64> = operands.iter().filter_map(Value::number).collect();
            let mut shown: Vec<&Value> = Vec::new();
            match operator.as_str() {
                "q" => saved.push(ctm),
                "Q" => ctm = saved.pop().unwrap_or(ctm),
                "cm" if numbers.len() == 6 => {
                    let matrix: Matrix = numbers[..6].try_into().expect("six numbers");
                    ctm = concat(&matrix, &ctm);
                }
                "BT" => (text_matrix, line_matrix) = (IDENTITY, IDENTITY),
                "Tf" => {
                    size = numbers.first().copied().unwrap_or(size);
                    font = operands.first().and_then(Value::name).and_then(|name| {
                        let Value::Ref(number) = font_refs.get(name)? else {
                            return None;
                        };
                        if !self.fonts.contains_key(number) {
                            let loaded = match pdf.object(*number) {
                                Some(Value::Dict(dict)) => Font::load(pdf, &dict),
                                _ => Font::default(),
                            };
                            self.fonts.insert(*number, loaded);
                        }
                        Some(*number)
                    });
                }
                "TL" => leading = numbers.first().copied().unwrap_or(leading),
                "Tc" => spacing = numbers.first().copied().unwrap_or(spacing),
                "Tw" => word_spacing = numbers.first().copied().unwrap_or(word_spacing),
                "Td" | "TD" if numbers.len() == 2 => {
                    if operator == "TD" {
                        leading = -numbers[1];
                    }
                    line_matrix = concat(&[1., 0., 0., 1., numbers[0], numbers[1]], &line_matrix);
                    text_matrix = line_matrix;
                }
                "Tm" if numbers.len() == 6 => {
                    line_matrix = numbers[..6].try_into().expect("six numbers");
                    text_matrix = line_matrix;
                }
                "T*" | "'" | "\"" => {
                    line_matrix = concat(&[1., 0., 0., 1., 0., -leading], &line_matrix);
                    text_matrix = line_matrix;
                    if operator != "T*" {
                        shown.extend(operands.last());
                    }
                }
                "Tj" => shown.extend(operands.first()),
                "TJ" => {
                    if let Some(Value::Array(parts)) = operands.first() {
                        shown.extend(parts.iter());
                    }
                }
                "BI" => {
                    // Inline image: skip its data.
                    let end = find(data, b"ID", parser.at)
                        .and_then(|data_start| {
                            let mut at = data_start;
                            loop {
                                at = find(data, b"EI", at + 2)?;
                                let before = data.get(at - 1).copied().is_some_and(is_space);
                                let after = data.get(at + 2).copied().is_none_or(is_space);
                                if before && after {
                                    return Some(at + 2);
                                }
                            }
                        })
                        .unwrap_or(data.len());
                    parser.at = end;
                }
                "Do" => {
                    let target = operands.first().and_then(Value::name).and_then(|name| {
                        match objects.get(name)? {
                            Value::Ref(number) => Some(*number),
                            _ => None,
                        }
                    });
                    if let Some(number) = target {
                        self.object(number, resources, ctm, depth);
                    }
                }
                _ => {}
            }
            let used = font
                .and_then(|number| self.fonts.get(&number))
                .unwrap_or(&inline);
            let mut pieces: Vec<(String, f64, f64, f64, f64)> = Vec::new();
            for part in shown {
                match part {
                    Value::Text(bytes) => {
                        let mut text = String::new();
                        let advance = used.decode(bytes, &mut text);
                        let count = (bytes.len() / used.width.max(1)) as f64;
                        let spaces = bytes.iter().filter(|byte| **byte == b' ').count() as f64;
                        let moved = advance / 1000. * size
                            + count * spacing
                            + if used.width == 1 {
                                spaces * word_spacing
                            } else {
                                0.
                            };
                        let place = concat(&text_matrix, &ctm);
                        let scale = (place[0] * place[0] + place[1] * place[1]).sqrt();
                        let height = (place[2] * place[2] + place[3] * place[3]).sqrt();
                        pieces.push((text, place[4], place[5], size.abs() * height, moved * scale));
                        text_matrix = concat(&[1., 0., 0., 1., moved, 0.], &text_matrix);
                    }
                    Value::Number(adjust) => {
                        text_matrix =
                            concat(&[1., 0., 0., 1., -adjust / 1000. * size, 0.], &text_matrix);
                    }
                    _ => {}
                }
            }
            for (text, x, y, height, moved) in pieces {
                // Text too small to read or turned on its side is decoration.
                if height >= 1. {
                    self.show(&text, x, y, height);
                    self.pen = (x + moved, height);
                }
            }
            operands.clear();
        }
    }

    fn object(&mut self, number: u32, resources: &Dict, ctm: Matrix, depth: u8) {
        let pdf = self.pdf;
        let Some((dict, raw)) = pdf.raw_stream(number) else {
            return;
        };
        match dict.get("Subtype").and_then(Value::name) {
            Some("Image") => {
                let side = |key: &str| pdf.get(&dict, key).number().unwrap_or(0.);
                if side("Width") < 100. || side("Height") < 100. || !self.seen.insert(number) {
                    return;
                }
                let image = if pdf.filters(&dict) == ["DCTDecode"] {
                    Some(raw.to_vec())
                } else {
                    pdf.png(number)
                };
                if let Some(image) = image {
                    self.end_line();
                    self.items.push(Item::Image(image));
                }
            }
            Some("Form") if depth < 6 => {
                let Some((_, data)) = pdf.stream(number) else {
                    return;
                };
                let own = pdf.dict(&dict, "Resources");
                let matrix = match pdf.get(&dict, "Matrix") {
                    Value::Array(items) if items.len() == 6 => {
                        let numbers: Vec<f64> = items.iter().filter_map(Value::number).collect();
                        numbers.try_into().unwrap_or(IDENTITY)
                    }
                    _ => IDENTITY,
                };
                self.content(
                    &data,
                    own.as_ref().unwrap_or(resources),
                    concat(&matrix, &ctm),
                    depth + 1,
                );
            }
            _ => {}
        }
    }
}

impl Pdf<'_> {
    /// Page objects in reading order, each with the resources it inherits.
    fn pages(&self) -> Vec<(u32, Dict, Dict)> {
        let mut pages = Vec::new();
        let root = self
            .catalog
            .and_then(|catalog| match self.object(catalog)? {
                Value::Dict(catalog) => match catalog.get("Pages")? {
                    Value::Ref(pages) => Some(*pages),
                    _ => None,
                },
                _ => None,
            });
        let mut visited = HashSet::new();
        // Depth first, kids in order.
        let mut pending: Vec<(u32, Dict)> =
            root.map(|root| (root, Dict::new())).into_iter().collect();
        while let Some((number, inherited)) = pending.pop() {
            if !visited.insert(number) || visited.len() > 500_000 {
                continue;
            }
            let Some(Value::Dict(node)) = self.object(number) else {
                continue;
            };
            let resources = self.dict(&node, "Resources").unwrap_or(inherited);
            match self.get(&node, "Kids") {
                Value::Array(kids) => {
                    for kid in kids.iter().rev() {
                        if let Value::Ref(kid) = kid {
                            pending.push((*kid, resources.clone()));
                        }
                    }
                }
                _ => pages.push((number, node, resources)),
            }
        }
        if pages.is_empty() {
            for number in &self.loose_pages {
                if let Some(Value::Dict(page)) = self.object(*number) {
                    let resources = self.dict(&page, "Resources").unwrap_or_default();
                    pages.push((*number, page, resources));
                }
            }
        }
        pages
    }

    /// Top-level bookmarks: title and the object number of the page.
    fn outline(&self) -> Vec<(String, u32)> {
        let mut entries = Vec::new();
        let first = self
            .catalog
            .and_then(|catalog| match self.object(catalog)? {
                Value::Dict(catalog) => self.dict(&catalog, "Outlines"),
                _ => None,
            })
            .and_then(|outlines| outlines.get("First").cloned());
        let mut next = first;
        let mut visited = HashSet::new();
        while let Some(Value::Ref(number)) = next {
            if !visited.insert(number) || visited.len() > 10_000 {
                break;
            }
            let Some(Value::Dict(item)) = self.object(number) else {
                break;
            };
            let destination = match self.get(&item, "Dest") {
                Value::Null => self
                    .dict(&item, "A")
                    .map_or(Value::Null, |action| self.get(&action, "D")),
                destination => destination,
            };
            let page = match destination {
                Value::Array(parts) => match parts.first() {
                    Some(Value::Ref(page)) => Some(*page),
                    _ => None,
                },
                _ => None,
            };
            if let (Some(page), Value::Text(title)) = (page, self.get(&item, "Title")) {
                entries.push((text_string(&title), page));
            }
            next = item.get("Next").cloned();
        }
        entries
    }
}

/// A PDF text string: UTF-16 with a byte order mark, or PDFDocEncoding.
pub(super) fn text_string(bytes: &[u8]) -> String {
    let text = match bytes {
        [0xFE, 0xFF, rest @ ..] => utf16(rest),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => encoding_rs::WINDOWS_1252
            .decode_without_bom_handling(bytes)
            .0
            .into_owned(),
    };
    text.trim_matches(|c: char| c.is_control() || c.is_whitespace())
        .to_string()
}

// ---------------------------------------------------------------------------
// From placed lines to a book
// ---------------------------------------------------------------------------

/// A part of the book's content, in reading order.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Part {
    /// Level 2 or 3.
    Heading(u8, String),
    Paragraph(String),
    Image(Vec<u8>),
}

/// The content of a PDF, divided where its bookmarks say chapters begin.
pub(super) struct PdfBook {
    /// Chapter title and content. Without bookmarks there is one chapter.
    pub chapters: Vec<(String, Vec<Part>)>,
}

pub(super) fn read_book(data: &[u8]) -> Result<PdfBook> {
    let pdf = Pdf::parse(data)?;
    let pages = pdf.pages();
    let mut reader = Reader {
        pdf: &pdf,
        fonts: HashMap::new(),
        seen: HashSet::new(),
        items: Vec::new(),
        line: None,
        pen: (0., 0.),
    };
    let mut starts: HashMap<u32, usize> = HashMap::new();
    let mut page_items: Vec<Vec<Item>> = Vec::new();
    for (number, page, resources) in &pages {
        starts.insert(*number, page_items.len());
        let streams: Vec<u32> = match page.get("Contents") {
            Some(Value::Ref(number)) => match pdf.object(*number) {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| match part {
                        Value::Ref(number) => Some(*number),
                        _ => None,
                    })
                    .collect(),
                _ => vec![*number],
            },
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| match part {
                    Value::Ref(number) => Some(*number),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        // The parts of a page's content are one stream cut anywhere.
        let mut content = Vec::new();
        for stream in streams {
            if let Some((_, data)) = pdf.stream(stream) {
                content.extend_from_slice(&data);
                content.push(b'\n');
            }
        }
        reader.content(&content, resources, IDENTITY, 0);
        reader.end_line();
        page_items.push(std::mem::take(&mut reader.items));
    }

    // The size most of the text is set in, to tell headings from it.
    let mut sizes: HashMap<u64, usize> = HashMap::new();
    for item in page_items.iter().flatten() {
        if let Item::Line(line) = item {
            *sizes.entry((line.size * 2.).round() as u64).or_default() += line.text.len();
        }
    }
    let body = sizes
        .iter()
        .max_by_key(|(size, count)| (**count, **size))
        .map_or(10., |(size, _)| *size as f64 / 2.);

    let mut boundaries: Vec<(usize, String)> = pdf
        .outline()
        .into_iter()
        .filter_map(|(title, page)| Some((*starts.get(&page)?, title)))
        .collect();
    boundaries.dedup_by_key(|(page, _)| *page);
    boundaries.sort_by_key(|(page, _)| *page);

    let mut book = PdfBook {
        chapters: vec![(String::new(), Vec::new())],
    };
    let mut open: Option<String> = None;
    let mut previous: Option<(f64, f64, f64)> = None;
    let close = |open: &mut Option<String>, book: &mut PdfBook| {
        if let Some(text) = open.take() {
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                book.chapters
                    .last_mut()
                    .expect("a chapter")
                    .1
                    .push(Part::Paragraph(text));
            }
        }
    };
    for (index, items) in page_items.into_iter().enumerate() {
        if let Some((_, title)) = boundaries.iter().find(|(page, _)| *page == index) {
            close(&mut open, &mut book);
            if book.chapters.len() == 1 && book.chapters[0].1.is_empty() {
                book.chapters[0].0 = title.clone();
            } else {
                book.chapters.push((title.clone(), Vec::new()));
            }
        }
        // The usual distance between lines of this page.
        let mut gaps: Vec<f64> = items
            .windows(2)
            .filter_map(|pair| match pair {
                [Item::Line(first), Item::Line(second)] => Some((first.y - second.y).abs()),
                _ => None,
            })
            .filter(|gap| *gap > 0.5)
            .collect();
        gaps.sort_by(f64::total_cmp);
        let usual = gaps.get(gaps.len() / 2).copied().unwrap_or(body * 1.2);
        let mut first_on_page = true;
        for item in items {
            let line = match item {
                Item::Image(data) => {
                    close(&mut open, &mut book);
                    book.chapters
                        .last_mut()
                        .expect("a chapter")
                        .1
                        .push(Part::Image(data));
                    previous = None;
                    continue;
                }
                Item::Line(line) => line,
            };
            let text = line.text.trim();
            // Page numbers.
            if text.len() <= 4 && text.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if line.size >= body * 1.2 && text.chars().count() < 150 {
                close(&mut open, &mut book);
                let level = if line.size >= body * 1.5 { 2 } else { 3 };
                let parts = &mut book.chapters.last_mut().expect("a chapter").1;
                // A heading broken over lines is one heading.
                match (parts.last_mut(), previous) {
                    (Some(Part::Heading(same, title)), Some((_, y, size)))
                        if *same == level && (y - line.y).abs() < size * 1.6 =>
                    {
                        title.push(' ');
                        title.push_str(text);
                    }
                    _ => parts.push(Part::Heading(level, text.to_string())),
                }
                previous = Some((line.x, line.y, line.size));
                first_on_page = false;
                continue;
            }
            let continues = match (&open, previous) {
                (Some(text), Some((x, y, _))) => {
                    if first_on_page {
                        // A paragraph may run over the page break.
                        !text
                            .trim_end()
                            .ends_with(['.', '!', '?', ':', '"', '\u{201d}'])
                    } else {
                        let gap = y - line.y;
                        gap > 0. && gap < usual * 1.35 && line.x - x < line.size * 0.8
                    }
                }
                _ => false,
            };
            if !continues {
                close(&mut open, &mut book);
            }
            let paragraph = open.get_or_insert_with(String::new);
            let hyphenated = paragraph.ends_with('-')
                && text.starts_with(char::is_lowercase)
                && paragraph
                    .chars()
                    .rev()
                    .nth(1)
                    .is_some_and(char::is_alphabetic);
            if hyphenated {
                paragraph.pop();
            } else if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(text);
            previous = Some((line.x, line.y, line.size));
            first_on_page = false;
        }
    }
    close(&mut open, &mut book);
    book.chapters.retain(|(_, parts)| !parts.is_empty());
    // Without bookmarks, the largest headings are where chapters begin.
    let is_major = |part: &Part| matches!(part, Part::Heading(2, _));
    if boundaries.is_empty()
        && book.chapters.len() == 1
        && book.chapters[0]
            .1
            .iter()
            .filter(|part| is_major(part))
            .count()
            > 1
    {
        let (_, parts) = book.chapters.remove(0);
        for part in parts {
            match &part {
                Part::Heading(2, title) => book.chapters.push((title.clone(), Vec::new())),
                _ if book.chapters.is_empty() => book.chapters.push((String::new(), Vec::new())),
                _ => {}
            }
            book.chapters.last_mut().expect("a chapter").1.push(part);
        }
    }
    Ok(book)
}

/// The text of a PDF, one chapter per top-level bookmark.
pub fn text(path: &Path) -> Result<BookText> {
    let book = read_book(&std::fs::read(path)?)?;
    let chapters = book
        .chapters
        .into_iter()
        .map(|(title, parts)| Chapter {
            title,
            text: parts
                .iter()
                .filter_map(|part| match part {
                    Part::Heading(_, text) | Part::Paragraph(text) => Some(text.as_str()),
                    Part::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        })
        .filter(|chapter| !chapter.text.is_empty())
        .collect();
    Ok(BookText { chapters })
}

/// A cover for a PDF: the first page when it is a photograph.
pub fn cover(path: &Path) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    let pdf = Pdf::parse(&data).ok()?;
    let (_, _, resources) = pdf.pages().into_iter().next()?;
    let objects = pdf.dict(&resources, "XObject")?;
    let mut images: Vec<(u64, &[u8])> = objects
        .values()
        .filter_map(|object| match object {
            Value::Ref(number) => pdf.raw_stream(*number),
            _ => None,
        })
        .filter(|(dict, _)| pdf.filters(dict) == ["DCTDecode"])
        .filter_map(|(dict, raw)| {
            let side = |key: &str| pdf.get(&dict, key).number().unwrap_or(0.) as u64;
            (side("Width") >= 300 && side("Height") >= 400)
                .then(|| (side("Width") * side("Height"), raw))
        })
        .collect();
    images.sort_by_key(|(area, _)| std::cmp::Reverse(*area));
    images.first().map(|(_, raw)| raw.to_vec())
}
