//! Editing an unpacked EPUB: checking it, search and replace, managing its
//! files, spine and table of contents, and saving it back safely.
//!
//! The package document is edited as text, so everything in it that these
//! tools do not understand is kept exactly as it was.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

use super::epub::{self, resolve_href};
use crate::error::{Error, Result};

static ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"([\w:.-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap());
static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Za-z][\w:.-]*)\b[^<>]*>").unwrap());
static ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<(?:\w+:)?item\b[^<>]*>(?:\s*</(?:\w+:)?item>)?[ \t]*\r?\n?").unwrap()
});
static ITEMREF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<(?:\w+:)?itemref\b[^<>]*>(?:\s*</(?:\w+:)?itemref>)?[ \t]*\r?\n?").unwrap()
});
static XML_ENCODING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(\s*<\?xml\b[^>]*?\bencoding\s*=\s*["'])[^"']*(["'])"#).unwrap()
});
static STYLE_ELEMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<(?:\w+:)?style\b[^<>]*>(.*?)</(?:\w+:)?style\s*>").unwrap()
});
static CSS_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@import\s+["']([^"']+)["']"#).unwrap());
static NAV_TOC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)(<nav\b[^>]*\bepub:type\s*=\s*["'][^"']*\btoc\b[^"']*["'][^>]*>).*?</nav\s*>"#,
    )
    .unwrap()
});
static CSS_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"url\(\s*["']?([^"')]+)["']?\s*\)"#).unwrap());
static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<h([1-6])\b([^<>]*)>(.*?)</h[1-6]\s*>").unwrap());
static MARKUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());

const MEDIA_TYPES: [(&str, &str); 24] = [
    ("xhtml", "application/xhtml+xml"),
    ("html", "application/xhtml+xml"),
    ("htm", "application/xhtml+xml"),
    ("css", "text/css"),
    ("ncx", "application/x-dtbncx+xml"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("gif", "image/gif"),
    ("svg", "image/svg+xml"),
    ("webp", "image/webp"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("js", "text/javascript"),
    ("mp3", "audio/mpeg"),
    ("mp4", "video/mp4"),
    ("m4a", "audio/mp4"),
    ("smil", "application/smil+xml"),
    ("pls", "application/pls+xml"),
    ("xml", "application/xml"),
    ("opf", "application/oebps-package+xml"),
    ("txt", "text/plain"),
];

/// Media types that mean the same as the one this module would choose.
const EQUIVALENT: [(&str, &str); 8] = [
    ("font/ttf", "application/x-font-ttf"),
    ("font/ttf", "application/x-font-truetype"),
    ("font/otf", "application/vnd.ms-opentype"),
    ("font/otf", "application/x-font-opentype"),
    ("font/woff", "application/font-woff"),
    ("text/javascript", "application/javascript"),
    ("application/xhtml+xml", "text/html"),
    ("font/ttf", "application/font-sfnt"),
];

fn media_type_for(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    MEDIA_TYPES
        .iter()
        .find(|(known, _)| *known == extension)
        .map_or("application/octet-stream", |(_, media_type)| media_type)
}

fn same_media_type(expected: &str, actual: &str) -> bool {
    expected == actual
        || EQUIVALENT
            .iter()
            .any(|(ours, theirs)| *ours == expected && *theirs == actual)
        || (expected == "font/otf" && actual == "font/ttf")
        || (expected == "font/ttf" && actual == "font/otf")
}

fn is_text(media_type: &str) -> bool {
    media_type.starts_with("text/") || media_type.contains("xml") || media_type.contains("html")
}

fn is_html(media_type: &str) -> bool {
    media_type == "application/xhtml+xml" || media_type == "text/html"
}

fn attributes(tag: &str) -> Vec<(String, String, Range<usize>)> {
    ATTRIBUTE
        .captures_iter(tag)
        .map(|found| {
            let value = found.get(2).or(found.get(3)).unwrap();
            (
                found[1].to_string(),
                html_escape::decode_html_entities(value.as_str()).to_string(),
                value.range(),
            )
        })
        .collect()
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    attributes(tag)
        .into_iter()
        .find(|(found, _, _)| found == name)
        .map(|(_, value, _)| value)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn directory_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

/// The href that leads from a file in `from_directory` to `target`.
fn relative_href(from_directory: &str, target: &str) -> String {
    let from: Vec<&str> = from_directory
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let to: Vec<&str> = target.split('/').collect();
    let common = from
        .iter()
        .zip(to.iter().take(to.len().saturating_sub(1)))
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    parts.extend(&to[common..]);
    parts.join("/")
}

fn is_external(href: &str) -> bool {
    href.contains("://")
        || href.starts_with("mailto:")
        || href.starts_with("data:")
        || href.starts_with("tel:")
        || href.starts_with("javascript:")
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem found by [`EpubDir::check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    /// The file the problem is in, relative to the book's root.
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
    /// Whether [`EpubDir::fix`] repairs it.
    pub fixable: bool,
}

/// A file listed in the package document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    /// Path relative to the book's root.
    pub path: String,
    pub media_type: String,
    pub properties: String,
    /// Position in the reading order, when the file is part of it.
    pub spine: Option<usize>,
}

/// Where a search matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub file: String,
    pub line: usize,
    /// The matching line, trimmed.
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
    /// Only search files whose path contains this.
    pub file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub title: String,
    pub level: u8,
    /// Root-relative path, with `#fragment` when the entry has one.
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

/// A difference between two versions of a book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub file: String,
    pub kind: ChangeKind,
    /// For modified text files: lines only in the old and only in the new.
    pub lines: Option<(usize, usize)>,
}

/// An unpacked EPUB.
#[derive(Debug)]
pub struct EpubDir {
    root: PathBuf,
    /// Root-relative path of the package document.
    opf_path: String,
    opf: String,
}

impl EpubDir {
    pub fn open(root: &Path) -> Result<Self> {
        let container = fs::read(root.join("META-INF/container.xml")).map_err(|_| {
            Error::Other(format!(
                "{} is not an unpacked EPUB (no META-INF/container.xml)",
                root.display()
            ))
        })?;
        let opf_path = epub::package_path(&container)?;
        let opf = fs::read_to_string(safe(root, &opf_path)?)
            .map_err(|err| Error::Other(format!("cannot read {opf_path}: {err}")))?;
        Ok(Self {
            root: root.to_path_buf(),
            opf_path,
            opf,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn opf_path(&self) -> &str {
        &self.opf_path
    }

    fn opf_directory(&self) -> &str {
        directory_of(&self.opf_path)
    }

    fn path(&self, relative: &str) -> Result<PathBuf> {
        safe(&self.root, relative)
    }

    fn read(&self, relative: &str) -> Result<String> {
        let data = fs::read(self.path(relative)?)?;
        Ok(super::decode_markup(&data))
    }

    fn write(&self, relative: &str, text: &str) -> Result<()> {
        let path = self.path(relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Text is always written as UTF-8, so the declaration must say so.
        let text = XML_ENCODING.replace(text, "${1}utf-8${2}");
        Ok(fs::write(path, text.as_bytes())?)
    }

    fn save_opf(&self) -> Result<()> {
        self.write(&self.opf_path, &self.opf)
    }

    fn spine_ids(&self) -> Vec<String> {
        ITEMREF
            .find_iter(&self.opf)
            .filter_map(|tag| attribute(tag.as_str(), "idref"))
            .collect()
    }

    /// The files of the manifest, in the order they are listed.
    pub fn items(&self) -> Vec<Item> {
        let spine = self.spine_ids();
        ITEM.find_iter(&self.opf)
            .filter_map(|tag| {
                let tag = tag.as_str();
                let id = attribute(tag, "id").unwrap_or_default();
                let href = attribute(tag, "href")?;
                // Remote resources are not files of the book.
                if is_external(&href) {
                    return None;
                }
                Some(Item {
                    path: resolve_href(self.opf_directory(), &href),
                    media_type: attribute(tag, "media-type").unwrap_or_default(),
                    properties: attribute(tag, "properties").unwrap_or_default(),
                    spine: spine.iter().position(|idref| *idref == id),
                    id,
                })
            })
            .collect()
    }

    /// The files in reading order.
    pub fn spine(&self) -> Vec<Item> {
        let mut items: Vec<Item> = self
            .items()
            .into_iter()
            .filter(|item| item.spine.is_some())
            .collect();
        items.sort_by_key(|item| item.spine);
        items
    }

    fn item(&self, path: &str) -> Result<Item> {
        self.items()
            .into_iter()
            .find(|item| item.path == path)
            .ok_or_else(|| Error::Other(format!("{path} is not in the book's manifest")))
    }

    /// Every file on disk, relative to the root.
    fn files(&self) -> Vec<String> {
        walkdir::WalkDir::new(&self.root)
            .sort_by_file_name()
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| {
                let relative = entry.path().strip_prefix(&self.root).ok()?;
                Some(relative.to_string_lossy().replace('\\', "/"))
            })
            .collect()
    }

    fn version(&self) -> f32 {
        TAG.find_iter(&self.opf)
            .find(|tag| tag.as_str().starts_with("<package") || tag.as_str().contains(":package"))
            .and_then(|tag| attribute(tag.as_str(), "version"))
            .and_then(|version| version.trim().parse().ok())
            .unwrap_or(2.0)
    }

    // ------------------------------------------------------------------
    // Checking
    // ------------------------------------------------------------------

    /// Look for problems, most serious first.
    pub fn check(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut report = |severity, file: &str, line, message: String, fixable| {
            issues.push(Issue {
                severity,
                file: file.to_string(),
                line,
                message,
                fixable,
            });
        };
        let opf = self.opf_path.as_str();

        match fs::read(self.root.join("mimetype")) {
            Ok(data) if data == b"application/epub+zip" => {}
            Ok(_) => report(
                Severity::Error,
                "mimetype",
                None,
                "must contain exactly application/epub+zip".into(),
                true,
            ),
            Err(_) => report(Severity::Error, "mimetype", None, "is missing".into(), true),
        }
        if let Some((line, message)) = malformed(&self.opf) {
            report(
                Severity::Error,
                opf,
                Some(line),
                format!("not well-formed XML: {message}"),
                false,
            );
        }
        for (element, what) in [
            ("title", "a title"),
            ("language", "a language"),
            ("identifier", "an identifier"),
        ] {
            let pattern = Regex::new(&format!(r"(?s)<dc:{element}\b[^>]*>\s*[^<\s]")).unwrap();
            if !pattern.is_match(&self.opf) {
                report(
                    Severity::Warning,
                    opf,
                    None,
                    format!("the book has no {what} (dc:{element})"),
                    false,
                );
            }
        }

        let items = self.items();
        let on_disk: HashSet<String> = self.files().into_iter().collect();
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        for item in &items {
            if !ids.insert(item.id.as_str()) {
                report(
                    Severity::Error,
                    opf,
                    None,
                    format!("manifest id {:?} is used more than once", item.id),
                    false,
                );
            }
            if !paths.insert(item.path.as_str()) {
                report(
                    Severity::Error,
                    opf,
                    None,
                    format!("{} is listed in the manifest more than once", item.path),
                    false,
                );
            }
            if !on_disk.contains(&item.path) {
                report(
                    Severity::Error,
                    opf,
                    None,
                    format!("{} is in the manifest but the file is missing", item.path),
                    true,
                );
                continue;
            }
            let expected = media_type_for(&item.path);
            if expected != "application/octet-stream"
                && !same_media_type(expected, &item.media_type)
            {
                report(
                    Severity::Warning,
                    opf,
                    None,
                    format!(
                        "{} has media type {:?}; expected {expected}",
                        item.path, item.media_type
                    ),
                    true,
                );
            }
        }
        for file in &on_disk {
            if file != "mimetype"
                && file != opf
                && !file.starts_with("META-INF/")
                && !paths.contains(file.as_str())
            {
                report(
                    Severity::Warning,
                    file,
                    None,
                    "is not listed in the manifest".into(),
                    true,
                );
            }
        }

        let spine = self.spine_ids();
        if spine.is_empty() {
            report(
                Severity::Error,
                opf,
                None,
                "the spine is empty: the book has no reading order".into(),
                false,
            );
        }
        for idref in &spine {
            if !ids.contains(idref.as_str()) {
                report(
                    Severity::Error,
                    opf,
                    None,
                    format!("the spine refers to {idref:?}, which is not in the manifest"),
                    true,
                );
            }
        }
        if let Some(cover) = TAG
            .find_iter(&self.opf)
            .filter(|tag| attribute(tag.as_str(), "name").as_deref() == Some("cover"))
            .find_map(|tag| attribute(tag.as_str(), "content"))
        {
            if !ids.contains(cover.as_str()) {
                report(
                    Severity::Warning,
                    opf,
                    None,
                    format!("the cover refers to {cover:?}, which is not in the manifest"),
                    false,
                );
            }
        }

        // Anchors of every text document, for checking links into them.
        let mut texts: HashMap<&str, String> = HashMap::new();
        let mut anchors: HashMap<&str, HashSet<String>> = HashMap::new();
        for item in &items {
            if !on_disk.contains(&item.path) || !is_text(&item.media_type) {
                continue;
            }
            let Ok(text) = self.read(&item.path) else {
                continue;
            };
            if is_html(&item.media_type) {
                let found = TAG
                    .find_iter(&text)
                    .flat_map(|tag| attributes(tag.as_str()))
                    .filter(|(name, _, _)| name == "id" || name == "name" || name == "xml:id")
                    .map(|(_, value, _)| value)
                    .collect();
                anchors.insert(&item.path, found);
            }
            texts.insert(&item.path, text);
        }

        let mut has_toc = false;
        for item in &items {
            let Some(text) = texts.get(item.path.as_str()) else {
                continue;
            };
            let ncx = item.media_type == "application/x-dtbncx+xml";
            let nav = item.properties.split_whitespace().any(|p| p == "nav");
            has_toc |= ncx || nav;
            if item.media_type.contains("xml") {
                if let Some((line, message)) = malformed(text) {
                    report(
                        Severity::Error,
                        &item.path,
                        Some(line),
                        format!("not well-formed XML: {message}"),
                        false,
                    );
                }
            }
            let base = directory_of(&item.path);
            let mut references: Vec<(usize, String)> = Vec::new();
            if item.media_type == "text/css" {
                references.extend(
                    CSS_URL
                        .captures_iter(text)
                        .map(|found| (found.get(0).unwrap().start(), found[1].to_string())),
                );
            } else if is_html(&item.media_type) || ncx {
                for tag in TAG.find_iter(text) {
                    for (name, value, _) in attributes(tag.as_str()) {
                        if matches!(name.as_str(), "href" | "src" | "xlink:href" | "poster") {
                            references.push((tag.start(), value));
                        }
                    }
                }
            }
            for (offset, reference) in references {
                if reference.is_empty() || is_external(&reference) {
                    continue;
                }
                let line = Some(line_of(text, offset));
                let (file, fragment) = reference.split_once('#').unwrap_or((&reference, ""));
                let target = if file.is_empty() {
                    item.path.clone()
                } else {
                    resolve_href(base, file)
                };
                if !on_disk.contains(&target) {
                    report(
                        Severity::Error,
                        &item.path,
                        line,
                        format!("{reference} points to a file that does not exist"),
                        false,
                    );
                } else if !fragment.is_empty() {
                    let fragment = percent_encoding::percent_decode_str(fragment)
                        .decode_utf8_lossy()
                        .to_string();
                    if anchors
                        .get(target.as_str())
                        .is_some_and(|known| !known.contains(&fragment))
                    {
                        report(
                            Severity::Warning,
                            &item.path,
                            line,
                            format!("{reference} points to an anchor that does not exist"),
                            false,
                        );
                    }
                }
            }
        }
        if !has_toc {
            report(
                Severity::Warning,
                opf,
                None,
                "the book has no table of contents".into(),
                true,
            );
        }
        issues.sort_by(|a, b| (a.severity, &a.file, a.line).cmp(&(b.severity, &b.file, b.line)));
        issues
    }

    /// Repair what can be repaired without guessing: the mimetype file,
    /// manifest entries for missing or unlisted files, wrong media types,
    /// spine entries with no file, and a missing table of contents.
    /// Returns what was done.
    pub fn fix(&mut self) -> Result<Vec<String>> {
        let mut done = Vec::new();
        if fs::read(self.root.join("mimetype")).ok().as_deref() != Some(b"application/epub+zip") {
            fs::write(self.root.join("mimetype"), "application/epub+zip")?;
            done.push("wrote mimetype".to_string());
        }
        let on_disk: HashSet<String> = self.files().into_iter().collect();
        let items = self.items();
        for item in &items {
            if !on_disk.contains(&item.path) {
                self.forget(&item.id);
                done.push(format!("removed {} from the manifest", item.path));
                continue;
            }
            let expected = media_type_for(&item.path);
            if expected != "application/octet-stream"
                && !same_media_type(expected, &item.media_type)
            {
                self.set_item_attribute(&item.id, "media-type", expected);
                done.push(format!("set the media type of {} to {expected}", item.path));
            }
        }
        let listed: HashSet<&str> = items.iter().map(|item| item.path.as_str()).collect();
        for file in &on_disk {
            if file != "mimetype"
                && *file != self.opf_path
                && !file.starts_with("META-INF/")
                && !listed.contains(file.as_str())
            {
                self.list(file, "");
                done.push(format!("added {file} to the manifest"));
            }
        }
        let ids: HashSet<String> = self.items().into_iter().map(|item| item.id).collect();
        for idref in self.spine_ids() {
            if !ids.contains(&idref) {
                self.remove_itemref(&idref);
                done.push(format!("removed {idref} from the spine"));
            }
        }
        self.save_opf()?;
        let has_toc = self.items().iter().any(|item| {
            item.media_type == "application/x-dtbncx+xml"
                || item.properties.split_whitespace().any(|p| p == "nav")
        });
        if !has_toc {
            let entries = self.generate_toc(2)?;
            done.push(format!(
                "wrote a table of contents with {entries} entries from the headings"
            ));
        }
        Ok(done)
    }

    // ------------------------------------------------------------------
    // Package document edits
    // ------------------------------------------------------------------

    fn item_tag(&self, id: &str) -> Option<Range<usize>> {
        ITEM.find_iter(&self.opf)
            .find(|tag| attribute(tag.as_str(), "id").as_deref() == Some(id))
            .map(|tag| tag.range())
    }

    fn remove_itemref(&mut self, id: &str) {
        let found = ITEMREF
            .find_iter(&self.opf)
            .find(|tag| attribute(tag.as_str(), "idref").as_deref() == Some(id))
            .map(|tag| tag.range());
        if let Some(range) = found {
            self.opf.replace_range(range, "");
        }
    }

    /// Drop a manifest entry and its place in the spine.
    fn forget(&mut self, id: &str) {
        if let Some(range) = self.item_tag(id) {
            self.opf.replace_range(range, "");
        }
        self.remove_itemref(id);
    }

    fn set_item_attribute(&mut self, id: &str, name: &str, value: &str) {
        let Some(range) = self.item_tag(id) else {
            return;
        };
        let tag = self.opf[range.clone()].to_string();
        let existing = attributes(&tag)
            .into_iter()
            .find(|(found, _, _)| found == name);
        let updated = match existing {
            Some((_, _, value_range)) => {
                format!(
                    "{}{}{}",
                    &tag[..value_range.start],
                    escape(value),
                    &tag[value_range.end..]
                )
            }
            None => {
                let end = tag.find("/>").or_else(|| tag.find('>')).unwrap_or(0);
                format!(
                    "{} {name}=\"{}\"{}",
                    tag[..end].trim_end(),
                    escape(value),
                    &tag[end..]
                )
            }
        };
        self.opf.replace_range(range, &updated);
    }

    fn unused_id(&self, path: &str) -> String {
        let stem: String = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let stem = if stem.starts_with(|c: char| c.is_ascii_alphabetic()) {
            stem
        } else {
            format!("id_{stem}")
        };
        // Every id in the package, including remote items and metadata.
        let used: HashSet<String> = TAG
            .find_iter(&self.opf)
            .filter_map(|tag| attribute(tag.as_str(), "id"))
            .collect();
        let mut id = stem.clone();
        let mut number = 1;
        while used.contains(&id) {
            number += 1;
            id = format!("{stem}_{number}");
        }
        id
    }

    /// The namespace prefix the package uses on an element, with its colon.
    fn prefix_of(&self, element: &str) -> String {
        Regex::new(&format!(r"<(\w+:)?{element}\b"))
            .unwrap()
            .captures(&self.opf)
            .and_then(|found| found.get(1))
            .map(|prefix| prefix.as_str().to_string())
            .unwrap_or_default()
    }

    /// Add a file on disk to the manifest; returns its id.
    fn list(&mut self, path: &str, properties: &str) -> String {
        let id = self.unused_id(path);
        let properties = if properties.is_empty() {
            String::new()
        } else {
            format!(" properties=\"{properties}\"")
        };
        let prefix = self.prefix_of("manifest");
        let tag = format!(
            "    <{prefix}item id=\"{id}\" href=\"{}\" media-type=\"{}\"{properties}/>\n  ",
            escape(&relative_href(self.opf_directory(), path)),
            media_type_for(path)
        );
        let end = Regex::new(r"</(?:\w+:)?manifest>").unwrap();
        if let Some(found) = end.find(&self.opf) {
            let mut at = found.start();
            // Keep the closing tag's own indentation.
            while at > 0 && matches!(self.opf.as_bytes()[at - 1], b' ' | b'\t') {
                at -= 1;
            }
            self.opf.replace_range(at..found.start(), &tag);
        }
        id
    }

    fn append_itemref(&mut self, id: &str, position: Option<usize>) {
        let prefix = self.prefix_of("spine");
        let tag = format!("    <{prefix}itemref idref=\"{id}\"/>\n");
        let existing: Vec<Range<usize>> = ITEMREF
            .find_iter(&self.opf)
            .map(|tag| tag.range())
            .collect();
        match position.and_then(|position| existing.get(position)) {
            Some(range) => self.opf.insert_str(range.start, tag.trim_start()),
            None => {
                let at = match existing.last() {
                    Some(last) => last.end,
                    None => Regex::new(r"</(?:\w+:)?spine>")
                        .unwrap()
                        .find(&self.opf)
                        .map_or(self.opf.len(), |found| found.start()),
                };
                let tag = if existing.is_empty() {
                    tag
                } else if self.opf[..at].ends_with('\n') {
                    format!("    {}", tag.trim_start())
                } else {
                    format!("\n    {}", tag.trim())
                };
                self.opf.insert_str(at, &tag);
            }
        }
    }

    // ------------------------------------------------------------------
    // Files
    // ------------------------------------------------------------------

    /// Copy a file into the book and list it; documents join the end of the
    /// reading order. `destination` is relative to the book's root.
    pub fn add_file(&mut self, source: &Path, destination: &str) -> Result<()> {
        let target = self.path(destination)?;
        if target.exists() {
            return Err(Error::Other(format!("{destination} already exists")));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, &target)?;
        let id = self.list(destination, "");
        if is_html(media_type_for(destination)) {
            self.append_itemref(&id, None);
        }
        self.save_opf()
    }

    /// Delete a file and take it out of the manifest and reading order.
    /// Links to it are left for [`EpubDir::check`] to report.
    pub fn remove_file(&mut self, path: &str) -> Result<()> {
        let item = self.item(path)?;
        let file = self.path(path)?;
        if file.exists() {
            fs::remove_file(file)?;
        }
        self.forget(&item.id);
        self.save_opf()
    }

    /// Rename or move a file and update every reference to it.
    /// Returns how many files had references changed.
    pub fn rename_file(&mut self, from: &str, to: &str) -> Result<usize> {
        let item = self.item(from)?;
        let target = self.path(to)?;
        if target.exists() {
            return Err(Error::Other(format!("{to} already exists")));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(self.path(from)?, &target)?;
        let href = relative_href(self.opf_directory(), to);
        self.set_item_attribute(&item.id, "href", &href);
        self.save_opf()?;

        let mut changed = 0;
        for other in self.items() {
            if !is_text(&other.media_type) {
                continue;
            }
            let moved = other.path == to;
            let Ok(text) = self.read(&other.path) else {
                continue;
            };
            // References are resolved from where the file was when written.
            let old_base = directory_of(if moved { from } else { &other.path });
            let new_base = directory_of(&other.path);
            let markup = other.media_type != "text/css";
            if let Some(text) = retarget(&text, markup, old_base, new_base, from, to, moved) {
                self.write(&other.path, &text)?;
                changed += 1;
            }
        }
        // Guide references in the package document.
        let base = self.opf_directory().to_string();
        if let Some(opf) = retarget(&self.opf, true, &base, &base, from, to, false) {
            self.opf = opf;
            self.save_opf()?;
        }
        Ok(changed)
    }

    /// Move a document to a position in the reading order (0 is first);
    /// a document outside the reading order joins it.
    pub fn move_in_spine(&mut self, path: &str, position: usize) -> Result<()> {
        let item = self.item(path)?;
        self.remove_itemref(&item.id);
        self.append_itemref(&item.id, Some(position));
        self.save_opf()
    }

    // ------------------------------------------------------------------
    // Search and replace
    // ------------------------------------------------------------------

    fn pattern(query: &str, options: &SearchOptions) -> Result<Regex> {
        let source = if options.regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        RegexBuilder::new(&source)
            .case_insensitive(!options.case_sensitive)
            .multi_line(true)
            .build()
            .map_err(|err| Error::Other(format!("bad search pattern: {err}")))
    }

    fn text_files(&self, options: &SearchOptions) -> Vec<String> {
        let mut files: Vec<String> = self
            .items()
            .into_iter()
            .filter(|item| is_text(&item.media_type))
            .map(|item| item.path)
            .collect();
        files.push(self.opf_path.clone());
        files.retain(|file| {
            options
                .file
                .as_ref()
                .is_none_or(|wanted| file.contains(wanted.as_str()))
        });
        files
    }

    /// Find text in the book's markup, styles and package document.
    pub fn search(&self, query: &str, options: &SearchOptions) -> Result<Vec<Match>> {
        let pattern = Self::pattern(query, options)?;
        let mut matches = Vec::new();
        if query.is_empty() {
            return Ok(matches);
        }
        for file in self.text_files(options) {
            let Ok(text) = self.read(&file) else {
                continue;
            };
            for found in pattern.find_iter(&text) {
                let start = text[..found.start()].rfind('\n').map_or(0, |at| at + 1);
                let end = text[found.start()..]
                    .find('\n')
                    .map_or(text.len(), |at| found.start() + at);
                matches.push(Match {
                    file: file.clone(),
                    line: line_of(&text, found.start()),
                    text: text[start..end].trim().to_string(),
                });
            }
        }
        Ok(matches)
    }

    /// Replace every match; with `regex`, `$1` in the replacement refers to
    /// a group. Returns the number of replacements per changed file.
    pub fn replace(
        &mut self,
        query: &str,
        replacement: &str,
        options: &SearchOptions,
    ) -> Result<Vec<(String, usize)>> {
        let pattern = Self::pattern(query, options)?;
        let mut changed = Vec::new();
        if query.is_empty() {
            return Ok(changed);
        }
        for file in self.text_files(options) {
            let Ok(text) = self.read(&file) else {
                continue;
            };
            let count = pattern.find_iter(&text).count();
            if count == 0 {
                continue;
            }
            let updated = if options.regex {
                pattern.replace_all(&text, replacement)
            } else {
                pattern.replace_all(&text, regex::NoExpand(replacement))
            };
            self.write(&file, &updated)?;
            if file == self.opf_path {
                self.opf = updated.to_string();
            }
            changed.push((file, count));
        }
        Ok(changed)
    }

    // ------------------------------------------------------------------
    // Table of contents
    // ------------------------------------------------------------------

    fn nav_item(&self) -> Option<Item> {
        self.items()
            .into_iter()
            .find(|item| item.properties.split_whitespace().any(|p| p == "nav"))
    }

    fn ncx_item(&self) -> Option<Item> {
        self.items()
            .into_iter()
            .find(|item| item.media_type == "application/x-dtbncx+xml")
    }

    /// The book's table of contents: the navigation document, or the NCX.
    pub fn toc(&self) -> Result<Vec<TocEntry>> {
        let mut entries = Vec::new();
        if let Some(nav) = self.nav_item() {
            let text = self.read(&nav.path)?;
            let base = directory_of(&nav.path);
            // Only the first <nav>: the others are landmarks and page lists.
            let body = Regex::new(r"(?is)<nav\b.*?</nav\s*>")
                .unwrap()
                .find(&text)
                .map_or(text.as_str(), |found| found.as_str());
            let token = Regex::new(r"(?is)<ol\b|</ol\s*>|<a\b[^>]*>.*?</a\s*>").unwrap();
            let mut level = 0u8;
            for found in token.find_iter(body) {
                let found = found.as_str();
                if found.starts_with("</") {
                    level = level.saturating_sub(1);
                } else if found[..3].eq_ignore_ascii_case("<ol") {
                    level = level.saturating_add(1);
                } else if let Some(href) = attribute(&found[..found.find('>').unwrap_or(0)], "href")
                {
                    entries.push(TocEntry {
                        title: plain(found),
                        level: level.max(1),
                        target: target_of(base, &href),
                    });
                }
            }
        } else if let Some(ncx) = self.ncx_item() {
            let text = self.read(&ncx.path)?;
            let base = directory_of(&ncx.path);
            let token = Regex::new(
                r"(?is)<navPoint\b|</navPoint\s*>|<text\b[^>]*>(.*?)</text\s*>|<content\b[^>]*>",
            )
            .unwrap();
            let mut level = 0u8;
            let mut title = String::new();
            for found in token.captures_iter(&text) {
                let whole = found.get(0).unwrap().as_str();
                if whole.starts_with("</") {
                    level = level.saturating_sub(1);
                } else if whole.to_ascii_lowercase().starts_with("<navpoint") {
                    level = level.saturating_add(1);
                } else if let Some(label) = found.get(1) {
                    title = plain(label.as_str());
                } else if level > 0 {
                    if let Some(source) = attribute(whole, "src") {
                        entries.push(TocEntry {
                            title: std::mem::take(&mut title),
                            level,
                            target: target_of(base, &source),
                        });
                    }
                }
            }
        }
        Ok(entries)
    }

    /// Rebuild the table of contents from the headings of the documents in
    /// reading order, down to `depth` (1 = only `h1`). Headings without an
    /// `id` are given one. Returns the number of entries.
    pub fn generate_toc(&mut self, depth: u8) -> Result<usize> {
        let mut entries = Vec::new();
        let mut counter = 0;
        for item in self.spine() {
            if !is_html(&item.media_type) || item.properties.contains("nav") {
                continue;
            }
            let Ok(text) = self.read(&item.path) else {
                continue;
            };
            let mut updated = String::with_capacity(text.len());
            let mut copied = 0;
            let mut found_any = false;
            for heading in HEADING.captures_iter(&text) {
                let level: u8 = heading[1].parse().unwrap_or(1);
                let title = plain(&heading[3]);
                if level > depth || title.is_empty() {
                    continue;
                }
                found_any = true;
                let id = match attribute(&heading[2], "id") {
                    Some(id) => id,
                    None => {
                        let id = loop {
                            counter += 1;
                            let id = format!("toc-{counter}");
                            if !text.contains(&format!("\"{id}\"")) {
                                break id;
                            }
                        };
                        let at = heading.get(2).unwrap().start();
                        updated.push_str(&text[copied..at]);
                        updated.push_str(&format!(" id=\"{id}\""));
                        copied = at;
                        id
                    }
                };
                entries.push(TocEntry {
                    title,
                    level,
                    target: format!("{}#{id}", item.path),
                });
            }
            if copied > 0 {
                updated.push_str(&text[copied..]);
                self.write(&item.path, &updated)?;
            }
            if !found_any && entries.is_empty() {
                // A book that opens without a heading still starts somewhere.
                entries.push(TocEntry {
                    title: "Start".into(),
                    level: 1,
                    target: item.path.clone(),
                });
            }
        }
        // Levels start at 1 and never skip one going deeper.
        let top = entries.iter().map(|entry| entry.level).min().unwrap_or(1);
        let mut previous = 0;
        for entry in &mut entries {
            entry.level = (entry.level - top + 1).min(previous + 1);
            previous = entry.level;
        }
        self.set_toc(&entries)?;
        Ok(entries.len())
    }

    /// Write a table of contents: the navigation document for EPUB 3, and
    /// the NCX when the book has one or is EPUB 2.
    pub fn set_toc(&mut self, entries: &[TocEntry]) -> Result<()> {
        let title = Regex::new(r"(?s)<dc:title\b[^>]*>(.*?)</dc:title>")
            .unwrap()
            .captures(&self.opf)
            .map(|found| found[1].trim().to_string())
            .unwrap_or_default();
        let existing_nav = self.nav_item();
        let existing_ncx = self.ncx_item();
        if self.version() >= 3.0 || existing_nav.is_some() {
            let path = match &existing_nav {
                Some(item) => item.path.clone(),
                None => {
                    let directory = self.opf_directory();
                    let path = if directory.is_empty() {
                        "nav.xhtml".to_string()
                    } else {
                        format!("{directory}/nav.xhtml")
                    };
                    if self.path(&path)?.exists() {
                        return Err(Error::Other(format!(
                            "{path} exists but is not the navigation document"
                        )));
                    }
                    path
                }
            };
            let base = directory_of(&path);
            let mut list = String::new();
            let mut level = 0;
            for entry in entries {
                while level < entry.level {
                    list.push_str("<ol>\n");
                    level += 1;
                }
                while level > entry.level {
                    list.push_str("</li>\n</ol>\n");
                    level -= 1;
                }
                if list.ends_with("</a>") || list.ends_with("</ol>\n") {
                    list.push_str("</li>\n");
                }
                list.push_str(&format!(
                    "<li><a href=\"{}\">{}</a>",
                    escape(&href_to(base, &entry.target)),
                    escape(&entry.title)
                ));
            }
            while level > 0 {
                list.push_str("</li>\n</ol>\n");
                level -= 1;
            }
            if entries.is_empty() {
                list.push_str("<ol></ol>\n");
            }
            let document = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n<head><title>{title}</title></head>\n<body>\n<nav epub:type=\"toc\" id=\"toc\">\n<h1>Contents</h1>\n{list}</nav>\n</body>\n</html>\n"
            );
            // Keep landmarks, page lists and any text of an existing document.
            let kept = existing_nav
                .as_ref()
                .and_then(|_| self.read(&path).ok())
                .filter(|text| NAV_TOC.is_match(text));
            let document = match kept {
                Some(text) => {
                    // The element's own tag stays, with its namespaces and id.
                    let open = NAV_TOC.captures(&text).unwrap()[1].to_string();
                    let nav = format!("{open}\n<h1>Contents</h1>\n{list}</nav>");
                    NAV_TOC.replace(&text, regex::NoExpand(&nav)).into_owned()
                }
                None => document,
            };
            self.write(&path, &document)?;
            if existing_nav.is_none() {
                self.list(&path, "nav");
            }
        }
        if existing_ncx.is_some() || self.version() < 3.0 {
            let path = match &existing_ncx {
                Some(item) => item.path.clone(),
                None => {
                    let directory = self.opf_directory();
                    if directory.is_empty() {
                        "toc.ncx".to_string()
                    } else {
                        format!("{directory}/toc.ncx")
                    }
                }
            };
            let base = directory_of(&path);
            let identifier = Regex::new(r"(?s)<dc:identifier\b[^>]*>(.*?)</dc:identifier>")
                .unwrap()
                .captures(&self.opf)
                .map(|found| found[1].trim().to_string())
                .unwrap_or_default();
            let mut map = String::new();
            let mut level = 0;
            for (number, entry) in entries.iter().enumerate() {
                while level >= entry.level {
                    map.push_str("</navPoint>\n");
                    level -= 1;
                }
                level = entry.level;
                map.push_str(&format!(
                    "<navPoint id=\"nav{0}\" playOrder=\"{0}\"><navLabel><text>{1}</text></navLabel><content src=\"{2}\"/>\n",
                    number + 1,
                    escape(&entry.title),
                    escape(&href_to(base, &entry.target))
                ));
            }
            while level > 0 {
                map.push_str("</navPoint>\n");
                level -= 1;
            }
            let depth = entries.iter().map(|entry| entry.level).max().unwrap_or(1);
            let document = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n<head>\n<meta name=\"dtb:uid\" content=\"{identifier}\"/>\n<meta name=\"dtb:depth\" content=\"{depth}\"/>\n</head>\n<docTitle><text>{title}</text></docTitle>\n<navMap>\n{map}</navMap>\n</ncx>\n"
            );
            self.write(&path, &document)?;
            if existing_ncx.is_none() {
                let id = self.list(&path, "");
                // EPUB 2 readers find the NCX through the spine's `toc`.
                let spine = Regex::new(r"<(?:\w+:)?spine\b[^>]*>").unwrap();
                if let Some(found) = spine.find(&self.opf) {
                    if attribute(found.as_str(), "toc").is_none() {
                        let at = found.end() - 1;
                        self.opf.insert_str(at, &format!(" toc=\"{id}\""));
                    }
                }
            }
        }
        self.save_opf()
    }

    // ------------------------------------------------------------------
    // Images
    // ------------------------------------------------------------------

    /// Re-encode JPEG and PNG images, scaling down those larger than
    /// `max_side` pixels. An image is only replaced when the result is
    /// smaller. Returns the bytes saved per changed image.
    pub fn compress_images(&self, quality: u8, max_side: u32) -> Result<Vec<(String, u64)>> {
        let mut saved = Vec::new();
        for item in self.items() {
            let jpeg = item.media_type == "image/jpeg";
            if !jpeg && item.media_type != "image/png" {
                continue;
            }
            let path = self.path(&item.path)?;
            let Ok(original) = fs::read(&path) else {
                continue;
            };
            let Ok(mut picture) = image::load_from_memory(&original) else {
                continue;
            };
            if picture.width().max(picture.height()) > max_side {
                picture = picture.resize(max_side, max_side, image::imageops::FilterType::Lanczos3);
            }
            let mut encoded = Vec::new();
            let result = if jpeg {
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality)
                    .encode_image(&picture.to_rgb8())
            } else {
                picture.write_with_encoder(image::codecs::png::PngEncoder::new_with_quality(
                    &mut encoded,
                    image::codecs::png::CompressionType::Best,
                    image::codecs::png::FilterType::Adaptive,
                ))
            };
            if result.is_ok() && encoded.len() < original.len() {
                fs::write(&path, &encoded)?;
                saved.push((item.path, (original.len() - encoded.len()) as u64));
            }
        }
        Ok(saved)
    }
}

fn safe(root: &Path, relative: &str) -> Result<PathBuf> {
    let unsafe_path = relative.is_empty()
        || Path::new(relative).is_absolute()
        || relative.split(['/', '\\']).any(|part| part == "..");
    if unsafe_path {
        return Err(Error::Other(format!("{relative:?} is outside the book")));
    }
    Ok(root.join(relative))
}

/// Text of a piece of markup.
fn plain(markup: &str) -> String {
    let text = MARKUP.replace_all(markup, " ");
    html_escape::decode_html_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn target_of(base: &str, href: &str) -> String {
    let (file, fragment) = href.split_once('#').unwrap_or((href, ""));
    let path = resolve_href(base, file);
    if fragment.is_empty() {
        path
    } else {
        format!("{path}#{fragment}")
    }
}

/// Rewrite the references in a file after `from` was renamed to `to`.
/// `moved` is set for the renamed file itself, whose own relative
/// references all change. Returns the new text when anything changed.
fn retarget(
    text: &str,
    markup: bool,
    old_base: &str,
    new_base: &str,
    from: &str,
    to: &str,
    moved: bool,
) -> Option<String> {
    let rewrite = |reference: &str| -> Option<String> {
        if reference.is_empty() || is_external(reference) || reference.starts_with('#') {
            return None;
        }
        let (file, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let resolved = resolve_href(old_base, file);
        let resolved = if resolved == from { to } else { &resolved };
        if resolved != to && !moved {
            return None;
        }
        let mut updated = relative_href(new_base, resolved);
        if !fragment.is_empty() {
            updated = format!("{updated}#{fragment}");
        }
        (updated != reference).then_some(updated)
    };
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    // Styles: a whole stylesheet, or the style elements and attributes of
    // markup. Attribute values are entity-encoded; other text is not touched.
    let mut styles: Vec<(Range<usize>, bool)> = Vec::new();
    if markup {
        styles.extend(
            STYLE_ELEMENT
                .captures_iter(text)
                .map(|found| (found.get(1).unwrap().range(), false)),
        );
        for tag in TAG.find_iter(text) {
            // Also SVG presentation attributes such as fill="url(...)".
            for (name, _, range) in attributes(tag.as_str()) {
                if matches!(
                    name.as_str(),
                    "style"
                        | "fill"
                        | "stroke"
                        | "filter"
                        | "mask"
                        | "clip-path"
                        | "marker"
                        | "marker-start"
                        | "marker-mid"
                        | "marker-end"
                ) {
                    styles.push((tag.start() + range.start..tag.start() + range.end, true));
                }
            }
        }
    } else {
        styles.push((0..text.len(), false));
    }
    for (style, encoded) in styles {
        let css = &text[style.clone()];
        for found in CSS_URL
            .captures_iter(css)
            .chain(CSS_IMPORT.captures_iter(css))
        {
            let reference = found.get(1).unwrap();
            let range = style.start + reference.start()..style.start + reference.end();
            if encoded {
                let decoded = html_escape::decode_html_entities(reference.as_str());
                if let Some(updated) = rewrite(&decoded) {
                    edits.push((range, escape(&updated)));
                }
            } else if let Some(updated) = rewrite(reference.as_str()) {
                edits.push((range, updated));
            }
        }
    }
    if markup {
        for tag in TAG.find_iter(text) {
            for (name, value, range) in attributes(tag.as_str()) {
                if matches!(name.as_str(), "href" | "src" | "xlink:href" | "poster") {
                    if let Some(updated) = rewrite(&value) {
                        edits.push((
                            tag.start() + range.start..tag.start() + range.end,
                            escape(&updated),
                        ));
                    }
                }
            }
        }
    }
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|(range, _)| range.start);
    edits.dedup_by_key(|(range, _)| range.start);
    let mut text = text.to_string();
    for (range, updated) in edits.into_iter().rev() {
        text.replace_range(range, &updated);
    }
    Some(text)
}

fn href_to(base: &str, target: &str) -> String {
    let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
    let href = relative_href(base, path);
    if fragment.is_empty() {
        href
    } else {
        format!("{href}#{fragment}")
    }
}

/// The first well-formedness error in an XML document: line and message.
fn malformed(text: &str) -> Option<(usize, String)> {
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Eof) => return None,
            Ok(quick_xml::events::Event::Start(tag) | quick_xml::events::Event::Empty(tag)) => {
                if let Some(Err(err)) = tag.attributes().find(|attribute| attribute.is_err()) {
                    let offset = (reader.buffer_position() as usize).saturating_sub(1);
                    return Some((line_of(text, offset), err.to_string()));
                }
            }
            Ok(_) => {}
            Err(err) => {
                let offset = reader.error_position() as usize;
                return Some((line_of(text, offset), err.to_string()));
            }
        }
    }
}

/// Pack an edited book into an EPUB without ever leaving a partial file:
/// it is written beside `output` and moved into place.
pub fn save(directory: &Path, output: &Path) -> Result<usize> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let scratch = tempfile::Builder::new()
        .prefix(".cr-save-")
        .suffix(".epub")
        .tempfile_in(parent)?;
    let count = epub::repack(directory, scratch.path())?;
    scratch
        .persist(output)
        .map_err(|err| Error::Other(format!("cannot write {}: {}", output.display(), err.error)))?;
    Ok(count)
}

fn archive_files(path: &Path) -> Result<HashMap<String, Vec<u8>>> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)?;
    let mut files = HashMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        files.insert(entry.name().to_string(), data);
    }
    Ok(files)
}

/// What differs between two EPUB files, by content rather than by bytes of
/// the archive.
pub fn compare(old: &Path, new: &Path) -> Result<Vec<Change>> {
    let before = archive_files(old)?;
    let after = archive_files(new)?;
    let mut changes = Vec::new();
    for (file, data) in &before {
        match after.get(file) {
            None => changes.push(Change {
                file: file.clone(),
                kind: ChangeKind::Removed,
                lines: None,
            }),
            Some(other) if other != data => {
                let lines = match (std::str::from_utf8(data), std::str::from_utf8(other)) {
                    (Ok(a), Ok(b)) => {
                        let mut counts: HashMap<&str, i64> = HashMap::new();
                        for line in a.lines() {
                            *counts.entry(line).or_default() += 1;
                        }
                        for line in b.lines() {
                            *counts.entry(line).or_default() -= 1;
                        }
                        let removed: i64 = counts.values().filter(|n| **n > 0).sum();
                        let added: i64 = -counts.values().filter(|n| **n < 0).sum::<i64>();
                        Some((removed as usize, added as usize))
                    }
                    _ => None,
                };
                changes.push(Change {
                    file: file.clone(),
                    kind: ChangeKind::Modified,
                    lines,
                });
            }
            Some(_) => {}
        }
    }
    for file in after.keys() {
        if !before.contains_key(file) {
            changes.push(Change {
                file: file.clone(),
                kind: ChangeKind::Added,
                lines: None,
            });
        }
    }
    changes.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_hrefs() {
        assert_eq!(relative_href("OEBPS", "OEBPS/text/a.xhtml"), "text/a.xhtml");
        assert_eq!(
            relative_href("OEBPS/text", "OEBPS/images/a.png"),
            "../images/a.png"
        );
        assert_eq!(relative_href("", "a.xhtml"), "a.xhtml");
        assert_eq!(relative_href("a/b", "c.css"), "../../c.css");
    }

    #[test]
    fn finds_malformed_xml() {
        assert_eq!(malformed("<a><b/></a>"), None);
        let (line, _) = malformed("<a>\n<b>\n</a>").unwrap();
        assert_eq!(line, 3);
    }
}
