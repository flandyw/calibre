//! Metadata (and where cheap, text) for document formats that are stored
//! rather than edited: DOCX, RTF, PDF and comic archives.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};
use crate::formats::txt::{decode_bytes, filename_metadata};
use crate::formats::{BookText, Chapter};
use crate::metadata::{htmlize_comments, parse_datetime, Metadata};
use crate::text::split_authors;
use crate::xml::{self, Node};

/// The largest archive member read into memory.
const MAX_ENTRY: u64 = 256 * 1024 * 1024;

fn zip_entry(archive: &mut zip::ZipArchive<File>, name: &str) -> Option<Vec<u8>> {
    let entry = archive.by_name(name).ok()?;
    let mut data = Vec::new();
    entry.take(MAX_ENTRY).read_to_end(&mut data).ok()?;
    Some(data)
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<File>> {
    zip::ZipArchive::new(File::open(path)?)
        .map_err(|error| Error::InvalidMetadata(format!("{}: {error}", path.display())))
}

fn set_text(target: &mut Option<String>, value: Option<String>) {
    if let Some(value) = value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        *target = Some(value);
    }
}

fn split_tags(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// DOCX
// ---------------------------------------------------------------------------

pub fn docx_metadata(path: &Path) -> Result<(Metadata, Option<Vec<u8>>)> {
    let mut meta = filename_metadata(path);
    let mut archive = open_zip(path)?;
    let core = zip_entry(&mut archive, "docProps/core.xml").and_then(|data| xml::parse(&data).ok());
    if let Some(core) = core {
        let field = |name: &str| core.child(name).map(Node::deep_text);
        if let Some(title) = field("title").filter(|title| !title.trim().is_empty()) {
            meta.title = title.trim().to_string();
        }
        if let Some(creator) = field("creator").filter(|c| !c.trim().is_empty()) {
            meta.authors = split_authors(&creator);
        }
        if let Some(keywords) = field("keywords") {
            meta.tags = split_tags(&keywords);
        }
        if let Some(description) = field("description").filter(|d| !d.trim().is_empty()) {
            meta.comments = Some(htmlize_comments(&description));
        }
        if let Some(language) = field("language").filter(|l| !l.trim().is_empty()) {
            meta.languages = vec![language.trim().to_string()];
        }
        meta.pubdate = field("created").and_then(|date| parse_datetime(&date));
    }
    let cover = zip_entry(&mut archive, "docProps/thumbnail.jpeg");
    Ok((meta, cover))
}

pub fn docx_text(path: &Path) -> Result<BookText> {
    let mut archive = open_zip(path)?;
    let data = zip_entry(&mut archive, "word/document.xml")
        .ok_or_else(|| Error::InvalidMetadata("not a DOCX file: no document.xml".into()))?;
    let root = xml::parse(&data)?;
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut current = Chapter::default();
    for paragraph in root.descendants("p") {
        let mut line = String::new();
        collect_runs(paragraph, &mut line);
        let style = paragraph
            .child("pPr")
            .and_then(|properties| properties.child("pStyle"))
            .and_then(|style| style.attr_any(&["w:val", "val"]))
            .unwrap_or_default();
        if matches!(style, "Heading1" | "Title") && !line.trim().is_empty() {
            if !current.text.trim().is_empty() || !current.title.is_empty() {
                chapters.push(std::mem::take(&mut current));
            }
            current.title = line.trim().to_string();
            continue;
        }
        current.text.push_str(line.trim_end());
        current.text.push_str("\n\n");
    }
    if !current.text.trim().is_empty() || chapters.is_empty() {
        chapters.push(current);
    }
    Ok(BookText { chapters })
}

fn collect_runs(node: &Node, out: &mut String) {
    for child in &node.children {
        match child.name.as_str() {
            "t" => out.push_str(&child.text),
            "tab" => out.push('\t'),
            "br" | "cr" => out.push('\n'),
            // Deleted text and field codes are not part of the document.
            "del" | "instrText" | "pPr" | "rPr" => {}
            _ => collect_runs(child, out),
        }
    }
}

// ---------------------------------------------------------------------------
// Comic archives (CBZ)
// ---------------------------------------------------------------------------

pub fn cbz_metadata(path: &Path) -> Result<(Metadata, Option<Vec<u8>>)> {
    let mut meta = filename_metadata(path);
    let mut archive = open_zip(path)?;
    let names: Vec<String> = archive.file_names().map(str::to_string).collect();

    let info_name = names
        .iter()
        .find(|name| name.eq_ignore_ascii_case("ComicInfo.xml"));
    let info = info_name
        .and_then(|name| zip_entry(&mut archive, name))
        .and_then(|data| xml::parse(&data).ok());
    if let Some(info) = info {
        let field = |name: &str| {
            info.child(name)
                .map(Node::deep_text)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let series = field("Series");
        let number = field("Number");
        match (field("Title"), &series, &number) {
            (Some(title), _, _) => meta.title = title,
            (None, Some(series), Some(number)) => meta.title = format!("{series} #{number}"),
            (None, Some(series), None) => meta.title = series.clone(),
            _ => {}
        }
        if let Some(writer) = field("Writer") {
            meta.authors = split_tags(&writer);
        }
        if let Some(index) = number.and_then(|number| number.parse::<f64>().ok()) {
            meta.series_index = index;
        }
        set_text(&mut meta.series, series);
        set_text(&mut meta.publisher, field("Publisher"));
        if let Some(genre) = field("Genre") {
            meta.tags = split_tags(&genre);
        }
        if let Some(summary) = field("Summary") {
            meta.comments = Some(htmlize_comments(&summary));
        }
        if let Some(language) = field("LanguageISO") {
            meta.languages = vec![language];
        }
    }

    // The first page, in reading order, is the cover.
    let mut pages: Vec<&String> = names
        .iter()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            !lower.starts_with("__macosx")
                && [".jpg", ".jpeg", ".png"]
                    .iter()
                    .any(|extension| lower.ends_with(extension))
        })
        .collect();
    pages.sort_by_key(|name| name.to_ascii_lowercase());
    let cover = pages.first().and_then(|name| zip_entry(&mut archive, name));
    Ok((meta, cover))
}

// ---------------------------------------------------------------------------
// RTF
// ---------------------------------------------------------------------------

/// Groups whose content is not document text.
const RTF_SKIPPED: [&str; 12] = [
    "fonttbl",
    "colortbl",
    "stylesheet",
    "pict",
    "object",
    "header",
    "footer",
    "footnote",
    "themedata",
    "datastore",
    "listtable",
    "generator",
];
const RTF_INFO: [&str; 5] = ["title", "author", "subject", "keywords", "doccomm"];

#[derive(Default)]
struct Rtf {
    body: String,
    /// `\info` fields in the order of [`RTF_INFO`].
    info: [String; 5],
}

#[derive(Clone, Copy, PartialEq)]
enum RtfTarget {
    Body,
    Skip,
    Info(usize),
}

fn parse_rtf(data: &[u8]) -> Rtf {
    let mut rtf = Rtf::default();
    // Each group has its own destination and `\uc` fallback length.
    let mut stack: Vec<(RtfTarget, usize)> = Vec::new();
    let mut target = RtfTarget::Body;
    let mut fallback = 1usize;
    let mut encoding = encoding_rs::WINDOWS_1252;
    // The first half of a UTF-16 surrogate pair.
    let mut high: Option<u16> = None;
    // Fallback characters still to skip after a `\u` escape.
    let mut skip = 0usize;
    let mut pending: Vec<u8> = Vec::new();
    let mut position = 0;

    fn flush(
        rtf: &mut Rtf,
        target: RtfTarget,
        pending: &mut Vec<u8>,
        encoding: &'static encoding_rs::Encoding,
    ) {
        if pending.is_empty() {
            return;
        }
        let (decoded, _, _) = encoding.decode(pending);
        match target {
            RtfTarget::Body => rtf.body.push_str(&decoded),
            RtfTarget::Info(field) => rtf.info[field].push_str(&decoded),
            RtfTarget::Skip => {}
        }
        pending.clear();
    }
    fn emit(rtf: &mut Rtf, target: RtfTarget, text: &str) {
        match target {
            RtfTarget::Body => rtf.body.push_str(text),
            RtfTarget::Info(field) => rtf.info[field].push_str(text),
            RtfTarget::Skip => {}
        }
    }

    while position < data.len() {
        let byte = data[position];
        position += 1;
        match byte {
            b'{' => {
                flush(&mut rtf, target, &mut pending, encoding);
                stack.push((target, fallback));
            }
            b'}' => {
                flush(&mut rtf, target, &mut pending, encoding);
                (target, fallback) = stack.pop().unwrap_or((RtfTarget::Body, 1));
            }
            b'\\' => {
                let Some(&next) = data.get(position) else {
                    break;
                };
                if next == b'\'' {
                    let hex = data.get(position + 1..position + 3);
                    let value = hex
                        .and_then(|hex| std::str::from_utf8(hex).ok())
                        .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                    position = (position + 3).min(data.len());
                    if skip > 0 {
                        skip -= 1;
                    } else if let Some(value) = value {
                        pending.push(value);
                    }
                    continue;
                }
                if !next.is_ascii_alphabetic() {
                    position += 1;
                    match next {
                        b'\\' | b'{' | b'}' => pending.push(next),
                        b'~' => pending.push(b' '),
                        b'*' => target = RtfTarget::Skip,
                        b'\n' | b'\r' => pending.push(b'\n'),
                        _ => {}
                    }
                    continue;
                }
                let start = position;
                while data.get(position).is_some_and(u8::is_ascii_alphabetic) {
                    position += 1;
                }
                let word = std::str::from_utf8(&data[start..position]).unwrap_or_default();
                let digits = position;
                if data.get(position) == Some(&b'-') {
                    position += 1;
                }
                while data.get(position).is_some_and(u8::is_ascii_digit) {
                    position += 1;
                }
                let number: Option<i32> = std::str::from_utf8(&data[digits..position])
                    .ok()
                    .and_then(|number| number.parse().ok());
                if data.get(position) == Some(&b' ') {
                    position += 1;
                }
                flush(&mut rtf, target, &mut pending, encoding);
                match word {
                    "par" | "line" | "sect" | "page" => emit(&mut rtf, target, "\n"),
                    "tab" => emit(&mut rtf, target, "\t"),
                    "emdash" => emit(&mut rtf, target, "—"),
                    "endash" => emit(&mut rtf, target, "–"),
                    "lquote" => emit(&mut rtf, target, "‘"),
                    "rquote" => emit(&mut rtf, target, "’"),
                    "ldblquote" => emit(&mut rtf, target, "“"),
                    "rdblquote" => emit(&mut rtf, target, "”"),
                    "u" => {
                        // A UTF-16 unit; negative values are it minus 65536.
                        let unit = number.unwrap_or(0).rem_euclid(65536) as u16;
                        let units = match high.take() {
                            Some(first) => vec![first, unit],
                            None => vec![unit],
                        };
                        match String::from_utf16(&units) {
                            Ok(text) => emit(&mut rtf, target, &text),
                            Err(_) if (0xD800..0xDC00).contains(&unit) => high = Some(unit),
                            Err(_) => {}
                        }
                        skip = fallback;
                    }
                    "uc" => fallback = number.unwrap_or(1).max(0) as usize,
                    "ansicpg" => {
                        let label = match number.unwrap_or(1252) {
                            932 => "shift_jis".to_string(),
                            936 => "gbk".to_string(),
                            949 => "euc-kr".to_string(),
                            950 => "big5".to_string(),
                            10000 => "macintosh".to_string(),
                            page => format!("windows-{page}"),
                        };
                        if let Some(found) = encoding_rs::Encoding::for_label(label.as_bytes()) {
                            encoding = found;
                        }
                    }
                    "bin" => position += number.unwrap_or(0).max(0) as usize,
                    "info" => target = RtfTarget::Skip,
                    word if target == RtfTarget::Skip => {
                        // Inside `\info`, the named fields are kept.
                        let in_info = stack.len() >= 2;
                        if let Some(field) = RTF_INFO.iter().position(|name| *name == word) {
                            if in_info && rtf.info[field].is_empty() {
                                target = RtfTarget::Info(field);
                            }
                        }
                    }
                    word if RTF_SKIPPED.contains(&word) => target = RtfTarget::Skip,
                    _ => {}
                }
            }
            b'\r' | b'\n' => {}
            _ if skip > 0 => skip -= 1,
            other => pending.push(other),
        }
    }
    flush(&mut rtf, target, &mut pending, encoding);
    rtf
}

fn read_rtf(path: &Path) -> Result<Rtf> {
    let data = std::fs::read(path)?;
    if !data.starts_with(b"{\\rtf") {
        return Err(Error::InvalidMetadata(format!(
            "{} is not an RTF file",
            path.display()
        )));
    }
    Ok(parse_rtf(&data))
}

pub fn rtf_metadata(path: &Path) -> Result<Metadata> {
    let mut meta = filename_metadata(path);
    let [title, author, subject, keywords, comment] = read_rtf(path)?.info;
    if !title.trim().is_empty() {
        meta.title = title.trim().to_string();
    }
    if !author.trim().is_empty() {
        meta.authors = split_authors(&author);
    }
    meta.tags = split_tags(&format!("{subject},{keywords}"));
    if !comment.trim().is_empty() {
        meta.comments = Some(htmlize_comments(&comment));
    }
    Ok(meta)
}

pub fn rtf_text(path: &Path) -> Result<BookText> {
    let body = read_rtf(path)?.body;
    let paragraphs: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    Ok(BookText {
        chapters: vec![Chapter {
            title: String::new(),
            text: paragraphs.join("\n\n"),
        }],
    })
}

// ---------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------

/// Read the document information dictionary. Only an uncompressed
/// dictionary is understood; other files keep their filename metadata.
pub fn pdf_metadata(path: &Path) -> Result<Metadata> {
    let mut meta = filename_metadata(path);
    let data = std::fs::read(path)?;
    let Some(info) = pdf_info_dictionary(&data) else {
        return Ok(meta);
    };
    let field = |key: &str| pdf_string(info, key).filter(|value| !value.trim().is_empty());
    if let Some(title) = field("/Title") {
        meta.title = title.trim().to_string();
    }
    if let Some(author) = field("/Author") {
        meta.authors = split_authors(&author);
    }
    if let Some(keywords) = field("/Keywords") {
        meta.tags = split_tags(&keywords);
    }
    if let Some(subject) = field("/Subject") {
        meta.comments = Some(htmlize_comments(&subject));
    }
    // `D:YYYYMMDD...`
    if let Some(date) = field("/CreationDate") {
        let digits: String = date.chars().filter(char::is_ascii_digit).take(8).collect();
        if digits.len() == 8 {
            let iso = format!("{}-{}-{}", &digits[..4], &digits[4..6], &digits[6..]);
            meta.pubdate = parse_datetime(&iso);
        }
    }
    Ok(meta)
}

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

fn rfind(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).rposition(|w| w == needle)
}

fn pdf_info_dictionary(data: &[u8]) -> Option<&[u8]> {
    // The newest trailer is the last one.
    let reference = rfind(data, b"/Info")? + 5;
    let rest = std::str::from_utf8(data.get(reference..(reference + 32).min(data.len()))?)
        .ok()
        .or_else(|| std::str::from_utf8(data.get(reference..reference + 12)?).ok())?;
    let mut parts = rest.split_whitespace();
    let object: u32 = parts.next()?.parse().ok()?;
    let generation: u32 = parts.next()?.parse().ok()?;
    let header = format!("{object} {generation} obj");
    // An updated file appends a new version of the object: the last wins.
    let mut newest = None;
    let mut from = 0;
    while let Some(at) = find(data, header.as_bytes(), from) {
        // `12 0 obj` must not match inside `112 0 obj`.
        if at == 0 || !data[at - 1].is_ascii_digit() {
            let start = at + header.len();
            if let Some(end) = find(data, b"endobj", start) {
                newest = Some(&data[start..end]);
            }
        }
        from = at + 1;
    }
    newest
}

fn pdf_string(dictionary: &[u8], key: &str) -> Option<String> {
    let mut from = 0;
    let start = loop {
        let at = find(dictionary, key.as_bytes(), from)? + key.len();
        // `/Title` must not match `/TitleFont`.
        if !dictionary.get(at).is_some_and(u8::is_ascii_alphanumeric) {
            break at;
        }
        from = at;
    };
    let rest = &dictionary[start..];
    let first = rest.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let mut bytes = Vec::new();
    match rest[first] {
        b'(' => {
            let mut depth = 1;
            let mut index = first + 1;
            while index < rest.len() {
                let byte = rest[index];
                index += 1;
                match byte {
                    b'\\' => {
                        let escaped = *rest.get(index)?;
                        index += 1;
                        match escaped {
                            b'n' => bytes.push(b'\n'),
                            b'r' => bytes.push(b'\r'),
                            b't' => bytes.push(b'\t'),
                            b'0'..=b'7' => {
                                let mut value = u32::from(escaped - b'0');
                                for _ in 0..2 {
                                    match rest.get(index) {
                                        Some(digit @ b'0'..=b'7') => {
                                            value = value * 8 + u32::from(digit - b'0');
                                            index += 1;
                                        }
                                        _ => break,
                                    }
                                }
                                bytes.push(value as u8);
                            }
                            b'\n' | b'\r' => {}
                            other => bytes.push(other),
                        }
                    }
                    b'(' => {
                        depth += 1;
                        bytes.push(byte);
                    }
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        bytes.push(byte);
                    }
                    other => bytes.push(other),
                }
            }
        }
        b'<' if rest.get(first + 1) != Some(&b'<') => {
            let end = find(rest, b">", first)?;
            let hex: Vec<u8> = rest[first + 1..end]
                .iter()
                .copied()
                .filter(u8::is_ascii_hexdigit)
                .collect();
            for pair in hex.chunks(2) {
                let text = std::str::from_utf8(pair).ok()?;
                bytes.push(u8::from_str_radix(&format!("{text:0<2}"), 16).ok()?);
            }
        }
        _ => return None,
    }
    // UTF-16 with a byte-order mark, otherwise (close enough to) Latin-1.
    Some(decode_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rtf_text_and_info() {
        let rtf = parse_rtf(
            br"{\rtf1\ansi{\fonttbl{\f0 Times;}}{\info{\title My Book}{\author Ann Lee}}
{\*\generator x;}\f0 Caf\'e9 \u8212?dash\par Second {\b bold} line\par}",
        );
        assert_eq!(rtf.info[0], "My Book");
        assert_eq!(rtf.info[1], "Ann Lee");
        assert_eq!(rtf.body, "Café —dash\nSecond bold line\n");

        let rtf = parse_rtf(
            br"{\rtf1\ansi\ansicpg1251 \'cf\'f0\'e8 {\uc2 \u8212 --x}\uc0 \u-10179 \u-8704 !\par}",
        );
        assert_eq!(rtf.body, "При —x😀!\n");
    }

    #[test]
    fn reads_pdf_information() {
        let pdf = b"%PDF-1.4\n11 0 obj\n<< /Title (Wrong) >>\nendobj\n\
            1 0 obj\n<< /Title (Dune \\(1965\\)) /Author <FEFF004C0065006D> /TitleFont /X >>\nendobj\n\
            trailer\n<< /Root 2 0 R /Info 1 0 R >>\n%%EOF";
        let info = pdf_info_dictionary(pdf).unwrap();
        assert_eq!(pdf_string(info, "/Title").unwrap(), "Dune (1965)");
        assert_eq!(pdf_string(info, "/Author").unwrap(), "Lem");
        assert!(pdf_string(info, "/Subject").is_none());
        assert!(pdf_info_dictionary(b"%PDF-1.7 no trailer").is_none());

        let mut updated = pdf.to_vec();
        updated.extend_from_slice(
            b"\n1 0 obj\n<< /Title (Revised) >>\nendobj\ntrailer\n<< /Info 1 0 R >>\n",
        );
        let info = pdf_info_dictionary(&updated).unwrap();
        assert_eq!(pdf_string(info, "/Title").unwrap(), "Revised");
    }
}
