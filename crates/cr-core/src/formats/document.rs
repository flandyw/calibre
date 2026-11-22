//! The document model every conversion goes through: sections of
//! well-formed XHTML, the images they use, a stylesheet and metadata.
//! Readers build it from a source format and writers produce a target from
//! it, so adding a format means one reader or one writer.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use base64::Engine;
use regex::Regex;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

use super::html::{attr_value, find_tag_end, first_heading, html_to_text, tag_name};
use super::{decode_markup, documents, epub, txt, BookText};
use crate::error::{Error, Result};
use crate::metadata::Metadata;
use crate::xml::{self, Node};

/// An image used by a document, stored under `images/` in the output.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub name: String,
    pub data: Vec<u8>,
}

/// One chapter-sized part of a document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Section {
    pub title: String,
    /// Well-formed XHTML body content. Images are `images/<name>` and links
    /// inside the book are `#fragment`, whichever section holds the target.
    pub html: String,
}

/// A book in the form conversions work on.
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub metadata: Metadata,
    pub cover: Option<Vec<u8>>,
    pub sections: Vec<Section>,
    pub images: Vec<Image>,
    /// Stylesheet carried over from the source.
    pub css: String,
    /// The source's own table of contents, when it has one.
    pub toc: Vec<TocItem>,
}

/// An entry of a source's table of contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocItem {
    pub title: String,
    /// 1 for top-level entries.
    pub level: u8,
    /// The `id` in a section that the entry points at.
    pub anchor: String,
}

/// How a reader divides a source into sections.
#[derive(Clone, Copy, PartialEq)]
enum Split {
    /// The caller already has one section per source file.
    None,
    /// At every `h1` and `h2`.
    Headings,
    /// At every FictionBook `<section>`.
    Fb2,
    /// FictionBook markup kept as one section.
    Fb2Whole,
}

/// The file extension and media type of an image, from its first bytes.
pub(super) fn image_type(data: &[u8]) -> Option<(&'static str, &'static str)> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if data.starts_with(b"\x89PNG") {
        Some(("png", "image/png"))
    } else if data.starts_with(b"GIF8") {
        Some(("gif", "image/gif"))
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else {
        let head = String::from_utf8_lossy(&data[..data.len().min(512)]);
        head.contains("<svg").then_some(("svg", "image/svg+xml"))
    }
}

/// The images of a document being read, deduplicated by where they came from.
#[derive(Default)]
struct Images {
    list: Vec<Image>,
    by_source: HashMap<String, Option<String>>,
}

impl Images {
    /// The `src` to use for an image, loading it on first use. `None` when
    /// the source has no usable image there.
    fn src(&mut self, source: &str, load: impl FnOnce() -> Option<Vec<u8>>) -> Option<String> {
        if let Some(known) = self.by_source.get(source) {
            return known.clone();
        }
        let src = load().and_then(|data| {
            let (extension, _) = image_type(&data)?;
            let name = format!("image{}.{extension}", self.list.len() + 1);
            let src = format!("images/{name}");
            self.list.push(Image { name, data });
            Some(src)
        });
        self.by_source.insert(source.to_string(), src.clone());
        src
    }
}

fn data_uri(uri: &str) -> Option<Vec<u8>> {
    let (header, payload) = uri.strip_prefix("data:")?.split_once(',')?;
    if !header.ends_with(";base64") {
        return None;
    }
    let payload: String = payload.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()
}

fn escape(text: &str) -> String {
    html_escape::encode_text(text).into_owned()
}

fn escape_attribute(text: &str) -> String {
    html_escape::encode_double_quoted_attribute(text).into_owned()
}

// ---------------------------------------------------------------------------
// Tolerant markup to well-formed XHTML
// ---------------------------------------------------------------------------

const VOID: [&str; 3] = ["br", "hr", "img"];
const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
/// Elements that end an open paragraph.
const BLOCKS: [&str; 22] = [
    "p",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "table",
    "tr",
    "blockquote",
    "pre",
    "hr",
    "figure",
    "figcaption",
    "caption",
];
/// Elements whose content is not part of the text.
const SKIPPED: [&str; 15] = [
    "script",
    "style",
    "noscript",
    "iframe",
    "object",
    "embed",
    "select",
    "textarea",
    "button",
    "template",
    "audio",
    "video",
    "canvas",
    // FictionBook
    "description",
    "binary",
];

impl Split {
    fn is_fb2(self) -> bool {
        matches!(self, Split::Fb2 | Split::Fb2Whole)
    }
}

/// The XHTML element a source element becomes; `None` keeps only its content.
fn map_tag(name: &str, split: Split) -> Option<&'static str> {
    const KEPT: [&str; 44] = [
        "p",
        "div",
        "span",
        "a",
        "em",
        "i",
        "strong",
        "b",
        "u",
        "s",
        "sub",
        "sup",
        "code",
        "pre",
        "blockquote",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "ul",
        "ol",
        "li",
        "dl",
        "dt",
        "dd",
        "table",
        "thead",
        "tbody",
        "tfoot",
        "tr",
        "td",
        "th",
        "caption",
        "br",
        "hr",
        "img",
        "figure",
        "figcaption",
        "small",
        "cite",
        "q",
        "tt",
    ];
    if split.is_fb2() {
        match name {
            "emphasis" => return Some("em"),
            "strikethrough" => return Some("s"),
            "empty-line" => return Some("br"),
            "title" => return Some("h2"),
            "subtitle" => return Some("h4"),
            "epigraph" | "cite" | "annotation" => return Some("blockquote"),
            "poem" | "stanza" => return Some("div"),
            "v" | "text-author" | "date" => return Some("p"),
            "image" => return Some("img"),
            _ => {}
        }
    }
    match name {
        "section" | "article" | "aside" | "header" | "footer" | "nav" | "main" | "center" => {
            (!split.is_fb2()).then_some("div")
        }
        "font" => Some("span"),
        "strike" | "del" => Some("s"),
        "image" => Some("img"),
        _ => KEPT.iter().find(|kept| **kept == name).copied(),
    }
}

struct Xhtml<'a> {
    split: Split,
    sections: Vec<Section>,
    out: String,
    stack: Vec<&'static str>,
    /// The current section has text outside headings.
    has_text: bool,
    ids: HashSet<String>,
    /// Gives an `id` the name it has in the whole document.
    rename_id: &'a dyn Fn(&str) -> String,
    css: String,
    image: &'a mut dyn FnMut(&str) -> Option<String>,
    link: &'a dyn Fn(&str) -> Option<String>,
}

impl Xhtml<'_> {
    fn run(&mut self, markup: &str) {
        let mut at = 0;
        while at < markup.len() {
            let Some(open) = markup[at..].find('<') else {
                self.text(&markup[at..]);
                break;
            };
            self.text(&markup[at..at + open]);
            at += open;
            let rest = &markup[at..];
            if rest.starts_with("<!--") {
                at += rest.find("-->").map_or(rest.len(), |end| end + 3);
            } else if let Some(data) = rest.strip_prefix("<![CDATA[") {
                let end = data.find("]]>").unwrap_or(data.len());
                self.raw_text(&data[..end]);
                at += (9 + end + 3).min(rest.len());
            } else if rest.starts_with("<!") || rest.starts_with("<?") {
                at += rest.find('>').map_or(rest.len(), |end| end + 1);
            } else if !rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/') {
                self.raw_text("<");
                at += 1;
            } else {
                let Some(end) = find_tag_end(rest) else {
                    break;
                };
                let tag = &rest[1..end];
                at += end + 1;
                let name = tag_name(tag);
                let skipped =
                    SKIPPED.contains(&name.as_str()) || (name == "title" && !self.split.is_fb2());
                if skipped && name != "embed" && !tag.starts_with('/') && !tag.ends_with('/') {
                    // Skip to the matching end tag; these do not nest. An
                    // element that is never closed hides nothing.
                    let lower = markup[at..].to_ascii_lowercase();
                    let close = format!("</{name}");
                    let Some(content_end) = lower.find(&close) else {
                        continue;
                    };
                    if name == "style" {
                        self.css.push_str(&markup[at..at + content_end]);
                        self.css.push('\n');
                    }
                    at += content_end;
                    at += markup[at..]
                        .find('>')
                        .map_or(markup.len() - at, |end| end + 1);
                } else {
                    self.tag(tag, &name);
                }
            }
        }
    }

    fn text(&mut self, text: &str) {
        self.raw_text(&html_escape::decode_html_entities(text));
    }

    fn raw_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !text.trim().is_empty() && !self.stack.iter().any(|open| HEADINGS.contains(open)) {
            self.has_text = true;
        }
        self.out.push_str(&escape(text));
    }

    fn close_through(&mut self, index: usize) {
        while self.stack.len() > index {
            let name = self.stack.pop().expect("stack is longer than index");
            self.out.push_str("</");
            self.out.push_str(name);
            self.out.push('>');
        }
    }

    /// Close `name` if it is open inside the nearest of `containers`.
    fn close_within(&mut self, names: &[&str], containers: &[&str]) {
        for index in (0..self.stack.len()).rev() {
            if containers.contains(&self.stack[index]) {
                return;
            }
            if names.contains(&self.stack[index]) {
                self.close_through(index);
                return;
            }
        }
    }

    fn finish_section(&mut self) {
        self.close_through(0);
        let html = std::mem::take(&mut self.out);
        if self.has_text
            || html.contains("<img ")
            || !super::html::strip_tags(&html).trim().is_empty()
        {
            self.sections.push(Section {
                title: first_heading(&html).unwrap_or_default(),
                html,
            });
        } else if let Some(previous) = self.sections.last_mut() {
            // Keeps anchors that links may point at.
            if html.contains(" id=\"") {
                previous.html.push_str(&html);
            }
        } else {
            // Nothing to attach it to yet: carry it into the next section.
            self.out = html;
        }
        self.has_text = false;
    }

    fn tag(&mut self, tag: &str, name: &str) {
        if let Some(closing) = tag.strip_prefix('/') {
            let _ = closing;
            let Some(mapped) = map_tag(name, self.split) else {
                return;
            };
            if let Some(index) = self.stack.iter().rposition(|open| *open == mapped) {
                self.close_through(index);
            }
            return;
        }
        let id = attr_value(tag, "id")
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty() && !id.contains(char::is_whitespace))
            .map(|id| (self.rename_id)(&id))
            .filter(|id| self.ids.insert(id.clone()));
        let starts_section = match self.split {
            Split::None | Split::Fb2Whole => false,
            Split::Headings => matches!(name, "h1" | "h2") && self.has_text,
            Split::Fb2 => name == "section" || name == "body",
        };
        if starts_section {
            self.finish_section();
        }
        let Some(mapped) = map_tag(name, self.split) else {
            if let Some(id) = id {
                self.out
                    .push_str(&format!("<a id=\"{}\"></a>", escape_attribute(&id)));
            }
            return;
        };
        let in_heading = self.stack.iter().any(|open| HEADINGS.contains(open));
        if in_heading && BLOCKS.contains(&mapped) && mapped != "hr" {
            // A heading holds only inline content: keep the words.
            if !self.out.ends_with(['>', ' ']) {
                self.out.push(' ');
            }
            return;
        }
        if BLOCKS.contains(&mapped) {
            self.close_within(&["p"], &["td", "th", "li", "div", "blockquote", "dd"]);
        }
        match mapped {
            "li" => self.close_within(&["li"], &["ul", "ol"]),
            "tr" => self.close_within(&["tr"], &["table"]),
            "td" | "th" => self.close_within(&["td", "th"], &["tr", "table"]),
            "dt" | "dd" => self.close_within(&["dt", "dd"], &["dl"]),
            "thead" | "tbody" | "tfoot" => {
                self.close_within(&["thead", "tbody", "tfoot"], &["table"])
            }
            _ => {}
        }
        let mut attributes = String::new();
        if let Some(id) = id {
            attributes.push_str(&format!(" id=\"{}\"", escape_attribute(&id)));
        }
        for keep in ["class", "lang", "dir", "title", "colspan", "rowspan"] {
            if let Some(value) = attr_value(tag, keep) {
                attributes.push_str(&format!(" {keep}=\"{}\"", escape_attribute(&value)));
            }
        }
        let href = ["href", "xlink:href", "l:href"]
            .iter()
            .find_map(|key| attr_value(tag, key));
        match mapped {
            "img" => {
                let source = attr_value(tag, "src").or(href);
                let Some(src) = source.and_then(|source| (self.image)(source.trim())) else {
                    return;
                };
                let alt = attr_value(tag, "alt").unwrap_or_default();
                self.out.push_str(&format!(
                    "<img{attributes} src=\"{}\" alt=\"{}\"/>",
                    escape_attribute(&src),
                    escape_attribute(&alt)
                ));
                return;
            }
            "a" => {
                if let Some(href) = href.and_then(|href| (self.link)(href.trim())) {
                    attributes.push_str(&format!(" href=\"{}\"", escape_attribute(&href)));
                }
            }
            _ => {}
        }
        self.out.push('<');
        self.out.push_str(mapped);
        self.out.push_str(&attributes);
        if VOID.contains(&mapped) {
            self.out.push_str("/>");
        } else if tag.ends_with('/') {
            self.out.push_str(&format!("></{mapped}>"));
        } else {
            self.out.push('>');
            self.stack.push(mapped);
        }
    }
}

/// Links that are safe to keep as written.
fn external_link(href: &str) -> Option<String> {
    let lower = href.to_ascii_lowercase();
    (href.starts_with('#')
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:"))
    .then(|| href.to_string())
}

/// Convert markup to sections. Returns the stylesheet found in it.
fn sections_from_markup(
    markup: &str,
    split: Split,
    ids: &mut HashSet<String>,
    rename_id: &dyn Fn(&str) -> String,
    image: &mut dyn FnMut(&str) -> Option<String>,
    link: &dyn Fn(&str) -> Option<String>,
) -> (Vec<Section>, String) {
    let mut xhtml = Xhtml {
        split,
        sections: Vec::new(),
        out: String::new(),
        stack: Vec::new(),
        has_text: false,
        ids: std::mem::take(ids),
        rename_id,
        css: String::new(),
        image,
        link,
    };
    xhtml.run(markup);
    xhtml.finish_section();
    *ids = xhtml.ids;
    (xhtml.sections, xhtml.css)
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

/// What a reader may change about how a source is read.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReadOptions {
    /// Start a section at each chapter heading.
    pub split_chapters: bool,
}

/// Read a book into the document model. Kindle books are read through
/// their EPUB form by the caller.
pub(super) fn read(path: &Path, format: &str, options: ReadOptions) -> Result<Document> {
    let mut metadata = super::import(path)?.metadata;
    let cover = metadata.cover.take();
    let mut document = Document {
        metadata,
        cover,
        ..Document::default()
    };
    let headings = if options.split_chapters {
        Split::Headings
    } else {
        Split::None
    };
    match format {
        "EPUB" => read_epub(path, &mut document)?,
        "HTML" => read_html(path, headings, &mut document)?,
        "FB2" => read_fb2(path, options.split_chapters, &mut document)?,
        "DOCX" => read_docx(path, options.split_chapters, &mut document)?,
        "TXT" => {
            let content = txt::decode_bytes(&std::fs::read(path)?);
            let text = if options.split_chapters {
                txt::split_chapters(&content, "")
            } else {
                BookText {
                    chapters: vec![super::Chapter {
                        title: String::new(),
                        text: content,
                    }],
                }
            };
            from_text(text, &mut document);
        }
        "RTF" => from_text(documents::rtf_text(path)?, &mut document),
        other => {
            return Err(Error::UnsupportedFormat(format!(
                "{other} cannot be converted"
            )))
        }
    }
    if document.sections.is_empty() {
        return Err(Error::Other(format!(
            "{} has no content to convert",
            path.display()
        )));
    }
    Ok(document)
}

/// Paragraphs of plain text: separated by blank lines, or one per line when
/// the text has none and its lines are not wrapped.
fn paragraphs(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let blocks: Vec<&str> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .collect();
    // Long lines are whole paragraphs; short ones are wrapped text.
    if blocks.len() == 1 && blocks[0].lines().any(|line| line.chars().count() > 120) {
        return blocks[0]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
    }
    blocks
        .iter()
        .map(|block| block.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

fn from_text(text: BookText, document: &mut Document) {
    for chapter in text.chapters {
        let mut html = String::new();
        if !chapter.title.trim().is_empty() {
            html.push_str(&format!("<h2>{}</h2>\n", escape(chapter.title.trim())));
        }
        let paragraphs = paragraphs(&chapter.text);
        if paragraphs.is_empty() {
            continue;
        }
        for paragraph in paragraphs {
            html.push_str(&format!("<p>{}</p>\n", escape(&paragraph)));
        }
        document.sections.push(Section {
            title: chapter.title.trim().to_string(),
            html,
        });
    }
}

fn read_html(path: &Path, split: Split, document: &mut Document) -> Result<()> {
    let markup = decode_markup(&std::fs::read(path)?);
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let mut images = Images::default();
    let mut image = |source: &str| {
        images.src(source, || {
            if source.starts_with("data:") {
                return data_uri(source);
            }
            if source.contains("://") {
                return None;
            }
            // Only files beside the page: a page must not pull in other
            // files of the user.
            let decoded = percent_encoding::percent_decode_str(source)
                .decode_utf8()
                .ok()?;
            let file = directory.join(decoded.as_ref()).canonicalize().ok()?;
            file.starts_with(&directory)
                .then(|| std::fs::read(file).ok())?
        })
    };
    let (sections, css) = sections_from_markup(
        &markup,
        split,
        &mut HashSet::new(),
        &str::to_string,
        &mut image,
        &external_link,
    );
    document.sections = sections;
    document.css = css;
    document.images = images.list;
    Ok(())
}

fn read_fb2(path: &Path, split: bool, document: &mut Document) -> Result<()> {
    let data = std::fs::read(path)?;
    let root = xml::parse(&data)?;
    let binaries: HashMap<&str, &Node> = root
        .children
        .iter()
        .filter(|child| child.name == "binary")
        .filter_map(|child| Some((child.attr("id")?, child)))
        .collect();
    let mut images = Images::default();
    let mut image = |source: &str| {
        images.src(source, || {
            let binary = binaries.get(source.trim_start_matches('#'))?;
            let encoded: String = binary.text.split_whitespace().collect();
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
    };
    let markup = decode_markup(&data);
    let (sections, _) = sections_from_markup(
        &markup,
        if split { Split::Fb2 } else { Split::Fb2Whole },
        &mut HashSet::new(),
        &str::to_string,
        &mut image,
        &external_link,
    );
    document.sections = sections;
    document.images = images.list;
    Ok(())
}

fn read_epub(path: &Path, document: &mut Document) -> Result<()> {
    let package = epub::read_package(path)?;
    let mut archive = ZipArchive::new(File::open(path)?)?;
    // Documents outside the reading order (notes, mostly) go last.
    let hrefs: Vec<&str> = package
        .spine
        .iter()
        .chain(&package.nonlinear)
        .filter_map(|idref| package.manifest.get(idref))
        .filter(|item| epub::is_html_media_type(&item.media_type))
        .map(|item| item.href.as_str())
        .collect();
    let mut documents: Vec<(&str, String)> = Vec::new();
    for href in hrefs {
        if documents.iter().any(|(seen, _)| *seen == href) {
            continue;
        }
        if let Ok(data) = epub::read_entry(&mut archive, href) {
            documents.push((href, decode_markup(&data)));
        }
    }
    let index: HashMap<&str, usize> = documents
        .iter()
        .enumerate()
        .map(|(index, (href, _))| (*href, index + 1))
        .collect();
    // An id used by several files gets the file's number appended, so that
    // every id is unique in the document and links keep their target.
    let id_pattern =
        Regex::new(r#"(?i)\sid\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("valid pattern");
    let mut first_use: HashMap<&str, usize> = HashMap::new();
    let mut shared: HashSet<&str> = HashSet::new();
    for (number, (_, markup)) in documents.iter().enumerate() {
        for captures in id_pattern.captures_iter(markup) {
            let id = captures
                .get(1)
                .or_else(|| captures.get(2))
                .map_or("", |id| id.as_str().trim());
            if *first_use.entry(id).or_insert(number) != number {
                shared.insert(id);
            }
        }
    }
    let unique = |id: &str, number: usize| {
        if shared.contains(id) {
            format!("{id}-cr{number}")
        } else {
            id.to_string()
        }
    };
    let mut stylesheets: Vec<&str> = package
        .manifest
        .values()
        .filter(|item| item.media_type == "text/css")
        .map(|item| item.href.as_str())
        .collect();
    stylesheets.sort_unstable();
    for href in stylesheets {
        if let Ok(css) = epub::read_entry(&mut archive, href) {
            document.css.push_str(&decode_markup(&css));
            document.css.push('\n');
        }
    }
    let mut images = Images::default();
    let mut ids = HashSet::new();
    for (number, (href, markup)) in documents.iter().enumerate() {
        let number = number + 1;
        let base = href.rsplit_once('/').map_or("", |(base, _)| base);
        let mut image = |source: &str| {
            let name = epub::resolve_href(base, source);
            images.src(&name, || {
                if source.starts_with("data:") {
                    return data_uri(source);
                }
                epub::read_entry(&mut archive, &name).ok()
            })
        };
        let link = |target: &str| {
            if let Some(fragment) = target.strip_prefix('#') {
                return Some(format!("#{}", unique(fragment, number)));
            }
            if target.contains(':') {
                return external_link(target);
            }
            // A link to another file of the book: every file starts with an
            // anchor.
            let (file, fragment) = target.split_once('#').unwrap_or((target, ""));
            let other = *index.get(epub::resolve_href(base, file).as_str())?;
            Some(if fragment.is_empty() {
                format!("#cr-doc-{other}")
            } else {
                format!("#{}", unique(fragment, other))
            })
        };
        let rename = |id: &str| unique(id, number);
        let (sections, css) =
            sections_from_markup(markup, Split::None, &mut ids, &rename, &mut image, &link);
        document.css.push_str(&css);
        for mut section in sections {
            section.html = format!("<a id=\"cr-doc-{number}\"></a>{}", section.html);
            document.sections.push(section);
        }
    }
    // The book's table of contents: the EPUB 3 navigation document, or the
    // EPUB 2 NCX.
    let anchor_of = |base: &str, target: &str| {
        let (file, fragment) = target.split_once('#').unwrap_or((target, ""));
        let number = *index.get(epub::resolve_href(base, file).as_str())?;
        let fragment = percent_encoding::percent_decode_str(fragment).decode_utf8_lossy();
        Some(if fragment.is_empty() {
            format!("cr-doc-{number}")
        } else {
            unique(&fragment, number)
        })
    };
    let mut contents: Vec<&epub::ManifestItem> = package
        .manifest
        .values()
        .filter(|item| item.is_nav() || item.media_type == "application/x-dtbncx+xml")
        .collect();
    contents.sort_by_key(|item| !item.is_nav());
    for item in contents {
        let Ok(data) = epub::read_entry(&mut archive, &item.href) else {
            continue;
        };
        let Ok(root) = xml::parse(&data) else {
            continue;
        };
        let base = item.href.rsplit_once('/').map_or("", |(base, _)| base);
        if item.is_nav() {
            let navs = root.descendants("nav");
            let nav = navs
                .iter()
                .find(|nav| nav.attr_any(&["epub:type", "type"]) == Some("toc"))
                .or(navs.first());
            if let Some(list) = nav.and_then(|nav| nav.child("ol")) {
                nav_items(list, 1, &mut |title, level, target| {
                    if let Some(anchor) = anchor_of(base, target) {
                        document.toc.push(TocItem {
                            title,
                            level,
                            anchor,
                        });
                    }
                });
            }
        } else if let Some(map) = root.child("navMap") {
            ncx_items(map, 1, &mut |title, level, target| {
                if let Some(anchor) = anchor_of(base, target) {
                    document.toc.push(TocItem {
                        title,
                        level,
                        anchor,
                    });
                }
            });
        }
        if !document.toc.is_empty() {
            break;
        }
    }
    document.images = images.list;
    Ok(())
}

fn node_text(node: &Node) -> String {
    let mut text = node.text.clone();
    for child in &node.children {
        text.push(' ');
        text.push_str(&node_text(child));
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Walk the nested `<ol>` of an EPUB 3 navigation document.
fn nav_items(list: &Node, level: u8, emit: &mut dyn FnMut(String, u8, &str)) {
    for item in list.children.iter().filter(|item| item.name == "li") {
        if let Some(link) = item.child("a") {
            if let Some(href) = link.attr("href") {
                emit(node_text(link), level, href);
            }
        }
        if let Some(nested) = item.child("ol") {
            nav_items(nested, level.saturating_add(1), emit);
        }
    }
}

/// Walk the nested `navPoint`s of an NCX.
fn ncx_items(parent: &Node, level: u8, emit: &mut dyn FnMut(String, u8, &str)) {
    for point in parent.children.iter().filter(|c| c.name == "navPoint") {
        let title = point.child("navLabel").map(node_text).unwrap_or_default();
        if let Some(source) = point.child("content").and_then(|c| c.attr("src")) {
            emit(title, level, source);
        }
        ncx_items(point, level.saturating_add(1), emit);
    }
}

fn docx_attr<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    node.attrs
        .iter()
        .find(|(key, _)| key.rsplit(':').next() == Some(name))
        .map(|(_, value)| value.as_str())
}

/// Whether a run property such as `<w:b/>` is switched on.
fn docx_on(properties: Option<&Node>, name: &str) -> bool {
    properties
        .and_then(|properties| properties.child(name))
        .is_some_and(|node| !matches!(docx_attr(node, "val"), Some("0" | "false" | "none")))
}

struct Docx<'a> {
    targets: HashMap<String, String>,
    archive: zip::ZipArchive<File>,
    images: &'a mut Images,
    /// Referenced notes as (kind, id), in the order of their markers.
    notes: Vec<(&'static str, String)>,
}

impl Docx<'_> {
    fn inline(&mut self, node: &Node, out: &mut String) {
        for child in &node.children {
            match child.name.as_str() {
                "r" => {
                    let properties = child.child("rPr");
                    let mut wrappers = Vec::new();
                    for (property, tag) in [("b", "strong"), ("i", "em"), ("strike", "s")] {
                        if docx_on(properties, property) {
                            wrappers.push(tag);
                        }
                    }
                    if docx_on(properties, "u") {
                        wrappers.push("u");
                    }
                    match properties
                        .and_then(|properties| properties.child("vertAlign"))
                        .and_then(|node| docx_attr(node, "val"))
                    {
                        Some("superscript") => wrappers.push("sup"),
                        Some("subscript") => wrappers.push("sub"),
                        _ => {}
                    }
                    let mut content = String::new();
                    self.inline(child, &mut content);
                    if content.is_empty() {
                        continue;
                    }
                    for tag in &wrappers {
                        out.push_str(&format!("<{tag}>"));
                    }
                    out.push_str(&content);
                    for tag in wrappers.iter().rev() {
                        out.push_str(&format!("</{tag}>"));
                    }
                }
                "t" => out.push_str(&escape(&child.text)),
                "footnoteReference" | "endnoteReference" => {
                    if let Some(id) = docx_attr(child, "id") {
                        let kind = if child.name == "footnoteReference" {
                            "footnote"
                        } else {
                            "endnote"
                        };
                        let known = self
                            .notes
                            .iter()
                            .position(|(seen, note)| *seen == kind && note == id);
                        let number = known.unwrap_or(self.notes.len()) + 1;
                        let mut anchor = String::new();
                        if known.is_none() {
                            self.notes.push((kind, id.to_string()));
                            anchor = format!(" id=\"cr-{kind}-ref-{}\"", escape_attribute(id));
                        }
                        out.push_str(&format!(
                            "<sup><a{anchor} href=\"#cr-{kind}-{}\">{number}</a></sup>",
                            escape_attribute(id),
                        ));
                    }
                }
                "tab" => out.push(' '),
                "br" | "cr" => out.push_str("<br/>"),
                "blip" => {
                    let target = docx_attr(child, "embed")
                        .and_then(|id| self.targets.get(id))
                        .cloned();
                    if let Some(target) = target {
                        let name = epub::resolve_href("word", &target);
                        let archive = &mut self.archive;
                        if let Some(src) = self
                            .images
                            .src(&name, || documents::zip_entry(archive, &name))
                        {
                            out.push_str(&format!("<img src=\"{src}\" alt=\"\"/>"));
                        }
                    }
                }
                "hyperlink" => {
                    let href = match docx_attr(child, "anchor") {
                        Some(anchor) => Some(format!("#{anchor}")),
                        None => docx_attr(child, "id")
                            .and_then(|id| self.targets.get(id))
                            .and_then(|target| external_link(target)),
                    };
                    let mut content = String::new();
                    self.inline(child, &mut content);
                    match href {
                        Some(href) => out.push_str(&format!(
                            "<a href=\"{}\">{content}</a>",
                            escape_attribute(&href)
                        )),
                        None => out.push_str(&content),
                    }
                }
                "bookmarkStart" => {
                    if let Some(name) = docx_attr(child, "name").filter(|name| *name != "_GoBack") {
                        out.push_str(&format!("<a id=\"{}\"></a>", escape_attribute(name)));
                    }
                }
                // Deleted text, field codes and properties are not content.
                "del" | "instrText" | "pPr" | "rPr" => {}
                _ => self.inline(child, out),
            }
        }
    }

    /// Render block content; returns a section break for each top-level
    /// heading when `split` is set.
    fn blocks(&mut self, node: &Node, split: bool, sections: &mut Vec<String>) {
        let mut in_list = false;
        for child in &node.children {
            let is_item = child.name == "p"
                && child
                    .child("pPr")
                    .is_some_and(|properties| properties.child("numPr").is_some());
            let out = sections.last_mut().expect("there is always a section");
            if in_list && !is_item {
                out.push_str("</ul>\n");
                in_list = false;
            }
            match child.name.as_str() {
                "p" => {
                    let style = child
                        .child("pPr")
                        .and_then(|properties| properties.child("pStyle"))
                        .and_then(|style| docx_attr(style, "val"))
                        .unwrap_or_default();
                    let level = match style {
                        "Title" => Some(1),
                        _ => style
                            .strip_prefix("Heading")
                            .and_then(|level| level.parse::<u8>().ok())
                            .filter(|level| (1..=6).contains(level)),
                    };
                    let mut content = String::new();
                    self.inline(child, &mut content);
                    if content.trim().is_empty() {
                        continue;
                    }
                    match level {
                        Some(level) => {
                            if split && level == 1 && !out.trim().is_empty() {
                                sections.push(String::new());
                            }
                            let out = sections.last_mut().expect("there is always a section");
                            out.push_str(&format!("<h{level}>{content}</h{level}>\n"));
                        }
                        None if is_item => {
                            if !in_list {
                                out.push_str("<ul>\n");
                                in_list = true;
                            }
                            out.push_str(&format!("<li>{content}</li>\n"));
                        }
                        None => out.push_str(&format!("<p>{content}</p>\n")),
                    }
                }
                "tbl" => {
                    let mut table = String::from("<table>\n");
                    for row in child.children.iter().filter(|row| row.name == "tr") {
                        table.push_str("<tr>");
                        for cell in row.children.iter().filter(|cell| cell.name == "tc") {
                            let mut inner = vec![String::new()];
                            self.blocks(cell, false, &mut inner);
                            table.push_str(&format!("<td>{}</td>", inner.concat()));
                        }
                        table.push_str("</tr>\n");
                    }
                    table.push_str("</table>\n");
                    out.push_str(&table);
                }
                "sdt" | "sdtContent" | "body" => self.blocks(child, split, sections),
                _ => {}
            }
        }
        if in_list {
            let out = sections.last_mut().expect("there is always a section");
            out.push_str("</ul>\n");
        }
    }
}

/// Relationship targets of a DOCX part, by relationship id.
fn docx_targets(
    archive: &mut zip::ZipArchive<File>,
    entry: &str,
) -> Result<HashMap<String, String>> {
    let mut targets = HashMap::new();
    if let Some(relationships) = documents::zip_entry(archive, entry) {
        for relationship in xml::parse(&relationships)?.descendants("Relationship") {
            if let (Some(id), Some(target)) = (relationship.attr("Id"), relationship.attr("Target"))
            {
                targets.insert(id.to_string(), target.to_string());
            }
        }
    }
    Ok(targets)
}

fn read_docx(path: &Path, split: bool, document: &mut Document) -> Result<()> {
    let mut archive = documents::open_zip(path)?;
    let data = documents::zip_entry(&mut archive, "word/document.xml")
        .ok_or_else(|| Error::InvalidMetadata("not a DOCX file: no document.xml".into()))?;
    let root = xml::parse(&data)?;
    let targets = docx_targets(&mut archive, "word/_rels/document.xml.rels")?;
    let mut images = Images::default();
    let mut docx = Docx {
        targets,
        archive,
        images: &mut images,
        notes: Vec::new(),
    };
    let mut sections = vec![String::new()];
    let body = root.child("body").unwrap_or(&root);
    docx.blocks(body, split, &mut sections);
    let referenced = std::mem::take(&mut docx.notes);
    let mut notes = String::new();
    for (kind, entry, relationships) in [
        (
            "footnote",
            "word/footnotes.xml",
            "word/_rels/footnotes.xml.rels",
        ),
        (
            "endnote",
            "word/endnotes.xml",
            "word/_rels/endnotes.xml.rels",
        ),
    ] {
        if !referenced.iter().any(|(seen, _)| *seen == kind) {
            continue;
        }
        let Some(data) = documents::zip_entry(&mut docx.archive, entry) else {
            continue;
        };
        // A notes part has its own relationships.
        docx.targets = docx_targets(&mut docx.archive, relationships)?;
        let root = xml::parse(&data)?;
        let bodies: HashMap<&str, &Node> = root
            .descendants(kind)
            .into_iter()
            // Separators are layout, not notes.
            .filter(|note| matches!(docx_attr(note, "type"), None | Some("normal")))
            .filter_map(|note| Some((docx_attr(note, "id")?, note)))
            .collect();
        for (number, (_, id)) in referenced
            .iter()
            .enumerate()
            .filter(|(_, (seen, _))| *seen == kind)
        {
            let Some(note) = bodies.get(id.as_str()) else {
                continue;
            };
            let mut content = vec![String::new()];
            docx.blocks(note, false, &mut content);
            let id = escape_attribute(id);
            notes.push_str(&format!(
                "<div id=\"cr-{kind}-{id}\"><p><a href=\"#cr-{kind}-ref-{id}\">{}.</a></p>{}</div>\n",
                number + 1,
                content.concat()
            ));
        }
    }
    if !notes.is_empty() {
        sections.push(format!("<h2>Notes</h2>\n{notes}"));
    }
    for html in sections {
        if !html.trim().is_empty() {
            document.sections.push(Section {
                title: first_heading(&html).unwrap_or_default(),
                html,
            });
        }
    }
    document.images = images.list;
    Ok(())
}

// ---------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------

impl Document {
    /// Scale images down to fit a screen, keeping their proportions.
    /// Images that cannot be decoded are left as they are.
    pub fn fit_images(&mut self, width: u32, height: u32) {
        let fit = |data: &mut Vec<u8>| {
            let Some((extension, _)) = image_type(data) else {
                return;
            };
            if !matches!(extension, "jpg" | "png") {
                return;
            }
            let Ok(image) = image::load_from_memory(data) else {
                return;
            };
            if image.width() <= width && image.height() <= height {
                return;
            }
            let scaled = image.resize(width, height, image::imageops::FilterType::Triangle);
            let mut output = Vec::new();
            let written = if extension == "png" {
                scaled.write_to(
                    &mut std::io::Cursor::new(&mut output),
                    image::ImageFormat::Png,
                )
            } else {
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 85)
                    .encode_image(&scaled.to_rgb8())
            };
            if written.is_ok() {
                *data = output;
            }
        };
        for image in &mut self.images {
            fit(&mut image.data);
        }
        if let Some(cover) = &mut self.cover {
            fit(cover);
        }
    }

    /// The section titles shown in the table of contents.
    fn toc(&self) -> Vec<String> {
        self.sections
            .iter()
            .enumerate()
            .map(|(index, section)| {
                if !section.title.trim().is_empty() {
                    section.title.trim().to_string()
                } else if self.sections.len() == 1 {
                    self.metadata.title.clone()
                } else {
                    format!("Section {}", index + 1)
                }
            })
            .collect()
    }

    fn language(&self) -> &str {
        self.metadata
            .languages
            .first()
            .map_or("und", |language| language.as_str())
    }
}

// ---------------------------------------------------------------------------
// Writers
// ---------------------------------------------------------------------------

const BASE_CSS: &str = "body { margin: 0 5%; line-height: 1.4; }\n\
p { margin: 0.6em 0; }\n\
h1, h2, h3 { text-align: center; margin: 1.5em 0 1em; }\n\
img { max-width: 100%; height: auto; }\n\
blockquote { margin: 1em 2em; }\n\
table { border-collapse: collapse; }\n\
td, th { border: 1px solid #888; padding: 0.2em 0.5em; }\n";

static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#" id="([^"]+)""#).unwrap());
static FRAGMENT_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r##" href="#([^"]+)""##).unwrap());

fn deflated() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

fn section_file(index: usize) -> String {
    format!("section{:04}.xhtml", index + 1)
}

fn xhtml_page(language: &str, title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" \
lang=\"{language}\" xml:lang=\"{language}\">\n<head>\n<title>{}</title>\n\
<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        escape(title),
        language = escape_attribute(language),
    )
}

fn package_opf(document: &Document, cover: Option<&str>) -> String {
    let meta = &document.metadata;
    let uuid = meta
        .uuid
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut opf = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"uid\">\n\
<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
<dc:identifier id=\"uid\">urn:uuid:{}</dc:identifier>\n<dc:title>{}</dc:title>\n\
<dc:language>{}</dc:language>\n<meta property=\"dcterms:modified\">{}</meta>\n",
        escape(&uuid),
        escape(&meta.title),
        escape(document.language()),
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
    );
    for author in &meta.authors {
        opf.push_str(&format!("<dc:creator>{}</dc:creator>\n", escape(author)));
    }
    if let Some(publisher) = &meta.publisher {
        opf.push_str(&format!(
            "<dc:publisher>{}</dc:publisher>\n",
            escape(publisher)
        ));
    }
    if let Some(date) = meta.pubdate {
        opf.push_str(&format!(
            "<dc:date>{}</dc:date>\n",
            date.format("%Y-%m-%dT%H:%M:%SZ")
        ));
    }
    if let Some(comments) = &meta.comments {
        opf.push_str(&format!(
            "<dc:description>{}</dc:description>\n",
            escape(comments)
        ));
    }
    for tag in &meta.tags {
        opf.push_str(&format!("<dc:subject>{}</dc:subject>\n", escape(tag)));
    }
    for (scheme, value) in &meta.identifiers {
        opf.push_str(&format!(
            "<dc:identifier>{}:{}</dc:identifier>\n",
            escape(scheme),
            escape(value)
        ));
    }
    if let Some(series) = &meta.series {
        opf.push_str(&format!(
            "<meta name=\"calibre:series\" content=\"{}\"/>\n\
<meta name=\"calibre:series_index\" content=\"{}\"/>\n",
            escape_attribute(series),
            crate::text::format_series_index(meta.series_index)
        ));
    }
    if cover.is_some() {
        opf.push_str("<meta name=\"cover\" content=\"cover\"/>\n");
    }
    opf.push_str("</metadata>\n<manifest>\n");
    opf.push_str(
        "<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n\
<item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n\
<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n",
    );
    if let Some(cover) = cover {
        let media_type = if cover.ends_with("png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        opf.push_str(&format!(
            "<item id=\"cover\" href=\"{cover}\" media-type=\"{media_type}\" properties=\"cover-image\"/>\n\
<item id=\"titlepage\" href=\"titlepage.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
        ));
    }
    for index in 0..document.sections.len() {
        opf.push_str(&format!(
            "<item id=\"section{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n",
            index + 1,
            section_file(index)
        ));
    }
    for (index, image) in document.images.iter().enumerate() {
        let media_type = image_type(&image.data).map_or("application/octet-stream", |kind| kind.1);
        opf.push_str(&format!(
            "<item id=\"image{}\" href=\"images/{}\" media-type=\"{media_type}\"/>\n",
            index + 1,
            escape_attribute(&image.name)
        ));
    }
    opf.push_str("</manifest>\n<spine toc=\"ncx\">\n");
    if cover.is_some() {
        opf.push_str("<itemref idref=\"titlepage\"/>\n");
    }
    for index in 0..document.sections.len() {
        opf.push_str(&format!("<itemref idref=\"section{}\"/>\n", index + 1));
    }
    opf.push_str("</spine>\n</package>\n");
    opf
}

/// Write the document as an EPUB 3 with an EPUB 2 table of contents.
pub fn write_epub(document: &Document, output: &mut File) -> Result<()> {
    let mut zip = ZipWriter::new(output);
    zip.start_file(
        "mimetype",
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )?;
    zip.write_all(b"application/epub+zip")?;
    zip.start_file("META-INF/container.xml", deflated())?;
    zip.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#)?;

    let cover = document.cover.as_ref().and_then(|data| {
        let (extension, _) = image_type(data).filter(|kind| matches!(kind.0, "jpg" | "png"))?;
        Some((format!("cover.{extension}"), data))
    });
    let language = document.language();
    zip.start_file("OEBPS/content.opf", deflated())?;
    zip.write_all(package_opf(document, cover.as_ref().map(|cover| cover.0.as_str())).as_bytes())?;
    zip.start_file("OEBPS/style.css", deflated())?;
    zip.write_all(BASE_CSS.as_bytes())?;
    zip.write_all(document.css.as_bytes())?;
    if let Some((name, data)) = &cover {
        zip.start_file(format!("OEBPS/{name}"), SimpleFileOptions::default())?;
        zip.write_all(data)?;
        zip.start_file("OEBPS/titlepage.xhtml", deflated())?;
        let body =
            format!("<div style=\"text-align: center\"><img src=\"{name}\" alt=\"Cover\"/></div>");
        zip.write_all(xhtml_page(language, "Cover", &body).as_bytes())?;
    }

    // A link may point into another section, which is another file here.
    let mut homes: HashMap<&str, usize> = HashMap::new();
    for (index, section) in document.sections.iter().enumerate() {
        for id in ID.captures_iter(&section.html) {
            homes
                .entry(id.get(1).expect("group 1 exists").as_str())
                .or_insert(index);
        }
    }
    let titles = document.toc();
    for (index, section) in document.sections.iter().enumerate() {
        let body = FRAGMENT_LINK.replace_all(&section.html, |found: &regex::Captures<'_>| {
            let fragment = &found[1];
            let decoded = html_escape::decode_html_entities(fragment);
            match homes.get(fragment).or_else(|| homes.get(decoded.as_ref())) {
                Some(home) if *home != index => {
                    format!(" href=\"{}#{fragment}\"", section_file(*home))
                }
                _ => found[0].to_string(),
            }
        });
        zip.start_file(format!("OEBPS/{}", section_file(index)), deflated())?;
        zip.write_all(xhtml_page(language, &titles[index], &body).as_bytes())?;
    }
    for image in &document.images {
        zip.start_file(
            format!("OEBPS/images/{}", image.name),
            SimpleFileOptions::default(),
        )?;
        zip.write_all(&image.data)?;
    }

    let mut nav = String::from("<nav epub:type=\"toc\" id=\"toc\">\n<h1>Contents</h1>\n<ol>\n");
    let mut ncx = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n<head>\n\
<meta name=\"dtb:uid\" content=\"urn:uuid:{}\"/>\n</head>\n\
<docTitle><text>{}</text></docTitle>\n<navMap>\n",
        escape_attribute(document.metadata.uuid.as_deref().unwrap_or_default()),
        escape(&document.metadata.title)
    );
    for (index, title) in titles.iter().enumerate() {
        let file = section_file(index);
        nav.push_str(&format!(
            "<li><a href=\"{file}\">{}</a></li>\n",
            escape(title)
        ));
        ncx.push_str(&format!(
            "<navPoint id=\"nav{0}\" playOrder=\"{0}\"><navLabel><text>{1}</text></navLabel>\
<content src=\"{file}\"/></navPoint>\n",
            index + 1,
            escape(title)
        ));
    }
    nav.push_str("</ol>\n</nav>");
    ncx.push_str("</navMap>\n</ncx>\n");
    zip.start_file("OEBPS/nav.xhtml", deflated())?;
    zip.write_all(xhtml_page(language, "Contents", &nav).as_bytes())?;
    zip.start_file("OEBPS/toc.ncx", deflated())?;
    zip.write_all(ncx.as_bytes())?;
    zip.finish()?;
    Ok(())
}

/// Write the document as HTMLZ: one HTML page, its stylesheet, images and
/// metadata in a zip file.
pub fn write_htmlz(document: &Document, output: &mut File) -> Result<()> {
    let mut zip = ZipWriter::new(output);
    let mut page = format!(
        "<!DOCTYPE html>\n<html lang=\"{}\">\n<head>\n<meta charset=\"utf-8\"/>\n<title>{}</title>\n\
<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n</head>\n<body>\n",
        escape_attribute(document.language()),
        escape(&document.metadata.title)
    );
    for section in &document.sections {
        page.push_str("<div class=\"cr-section\">\n");
        page.push_str(&section.html);
        page.push_str("\n</div>\n");
    }
    page.push_str("</body>\n</html>\n");
    zip.start_file("index.html", deflated())?;
    zip.write_all(page.as_bytes())?;
    zip.start_file("style.css", deflated())?;
    zip.write_all(BASE_CSS.as_bytes())?;
    zip.write_all(document.css.as_bytes())?;
    zip.start_file("metadata.opf", deflated())?;
    zip.write_all(crate::opf::metadata_to_opf(&document.metadata).as_bytes())?;
    if let Some(cover) = document.cover.as_deref().and_then(super::normalize_cover) {
        zip.start_file("cover.jpg", SimpleFileOptions::default())?;
        zip.write_all(&cover)?;
    }
    for image in &document.images {
        zip.start_file(
            format!("images/{}", image.name),
            SimpleFileOptions::default(),
        )?;
        zip.write_all(&image.data)?;
    }
    zip.finish()?;
    Ok(())
}

/// Write the document as plain text.
pub fn write_txt(document: &Document, output: &mut File) -> Result<()> {
    for section in &document.sections {
        writeln!(output, "{}\n", html_to_text(&section.html).trim())?;
    }
    Ok(())
}
