//! A book laid out for reading: sections of blocks with styled text,
//! images, links and a table of contents, built from a [`Document`].

use std::collections::HashMap;
use std::path::Path;

use regex::RegexBuilder;

use super::document::{Document, Image};
use super::html::{attr_value, find_tag_end, tag_name};
use crate::error::Result;

/// How a run of text inside a block is styled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Style {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Superscript,
    Subscript,
    /// `#anchor` inside the book, or an external address.
    Link(String),
}

/// A styled byte range of a block's text. Spans may overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// Level 1 to 6.
    Heading(u8),
    /// The marker is a bullet or the item's number.
    ListItem {
        marker: String,
    },
    Preformatted,
    /// Cells are separated by tabs in the block's text.
    TableRow,
    /// Index into [`Book::images`]; the block's text is the description.
    Image(usize),
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub spans: Vec<Span>,
    /// Nesting inside lists and quotations.
    pub indent: u8,
    pub quoted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadingSection {
    pub title: String,
    pub blocks: Vec<Block>,
}

/// A place in a book: a block and a byte offset into its text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Locator {
    pub section: usize,
    pub block: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub title: String,
    /// 1 for top-level entries.
    pub level: u8,
    pub target: Locator,
}

/// A search match inside a book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub start: Locator,
    /// Byte offset of the match's end in the same block.
    pub end: usize,
    /// The surrounding text, with the match wrapped in `[` and `]`.
    pub snippet: String,
}

#[derive(Debug, Clone, Default)]
pub struct Book {
    pub title: String,
    pub sections: Vec<ReadingSection>,
    pub toc: Vec<TocEntry>,
    pub images: Vec<Image>,
    anchors: HashMap<String, Locator>,
    /// Text length before each section, and the total as the last item.
    lengths: Vec<usize>,
}

const INLINE: [(&str, Style); 11] = [
    ("strong", Style::Bold),
    ("b", Style::Bold),
    ("em", Style::Italic),
    ("i", Style::Italic),
    ("cite", Style::Italic),
    ("u", Style::Underline),
    ("s", Style::Strike),
    ("code", Style::Code),
    ("tt", Style::Code),
    ("sup", Style::Superscript),
    ("sub", Style::Subscript),
];

/// Elements that end the block before them.
const BLOCK_TAGS: [&str; 15] = [
    "p",
    "div",
    "figure",
    "figcaption",
    "caption",
    "dt",
    "dd",
    "table",
    "blockquote",
    "pre",
    "ul",
    "ol",
    "li",
    "tr",
    "img",
];

struct Layout<'a> {
    images: &'a HashMap<&'a str, usize>,
    blocks: Vec<Block>,
    anchors: Vec<(String, usize)>,
    text: String,
    spans: Vec<Span>,
    /// Open inline styles and where each starts in the current block.
    open: Vec<(String, Style, usize)>,
    heading: Option<u8>,
    pre: bool,
    table_row: bool,
    quotes: u8,
    /// Open lists: the next number for an ordered one.
    lists: Vec<Option<usize>>,
    /// The marker of a list item that has no block yet.
    marker: Option<String>,
    pending_anchors: Vec<String>,
}

impl Layout<'_> {
    fn flush(&mut self, kind: Option<BlockKind>) {
        let end = self.text.trim_end().len();
        let empty = self.text.trim().is_empty();
        if empty && kind.is_none() {
            self.text.clear();
            self.spans.clear();
            for open in &mut self.open {
                open.2 = 0;
            }
            return;
        }
        // Styles still open continue in the next block.
        for (_, style, start) in &mut self.open {
            if *start < end {
                self.spans.push(Span {
                    start: *start,
                    end,
                    style: style.clone(),
                });
            }
            *start = 0;
        }
        let mut text = std::mem::take(&mut self.text);
        text.truncate(end);
        let mut spans = std::mem::take(&mut self.spans);
        spans.retain_mut(|span| {
            span.end = span.end.min(end);
            span.start < span.end
        });
        spans.sort_by_key(|span| (span.start, span.end));
        let kind = kind.unwrap_or_else(|| {
            if let Some(level) = self.heading {
                BlockKind::Heading(level)
            } else if self.table_row {
                BlockKind::TableRow
            } else if self.pre {
                BlockKind::Preformatted
            } else if let Some(marker) = self.marker.take() {
                BlockKind::ListItem { marker }
            } else {
                BlockKind::Paragraph
            }
        });
        for anchor in self.pending_anchors.drain(..) {
            self.anchors.push((anchor, self.blocks.len()));
        }
        self.blocks.push(Block {
            kind,
            text,
            spans,
            indent: self.lists.len().saturating_sub(1) as u8 + self.quotes,
            quoted: self.quotes > 0,
        });
    }

    fn push_text(&mut self, text: &str) {
        if self.pre {
            self.text.push_str(text);
            return;
        }
        for character in text.chars() {
            if character.is_whitespace() && character != '\u{a0}' {
                if !self.text.is_empty() && !self.text.ends_with([' ', '\n', '\t']) {
                    self.text.push(' ');
                }
            } else {
                self.text.push(character);
            }
        }
    }

    fn open_tag(&mut self, name: &str, tag: &str) {
        // An anchor on a block belongs to that block, not to text before it.
        let heading = name.len() == 2 && name.starts_with('h');
        if heading || BLOCK_TAGS.contains(&name) {
            self.flush(None);
        }
        if let Some(id) = attr_value(tag, "id") {
            self.pending_anchors.push(id);
        }
        if let Some(level) = name
            .strip_prefix('h')
            .and_then(|level| level.parse::<u8>().ok())
        {
            self.flush(None);
            self.heading = Some(level);
            return;
        }
        match name {
            "p" | "div" | "figure" | "figcaption" | "caption" | "dt" | "dd" | "table" => {
                self.flush(None)
            }
            "blockquote" => {
                self.flush(None);
                self.quotes += 1;
            }
            "pre" => {
                self.flush(None);
                self.pre = true;
            }
            "ul" | "ol" => {
                self.flush(None);
                self.lists.push((name == "ol").then_some(1));
            }
            "li" => {
                self.flush(None);
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        format!("{}.", *number - 1)
                    }
                    _ => "\u{2022}".to_string(),
                });
            }
            "tr" => {
                self.flush(None);
                self.table_row = true;
            }
            "td" | "th" => {
                if self.table_row && !self.text.is_empty() {
                    let end = self.text.trim_end().len();
                    self.text.truncate(end);
                    self.text.push('\t');
                }
            }
            "br" => {
                if self.pre || !self.text.is_empty() {
                    self.text.push('\n');
                }
            }
            "hr" => {
                self.flush(None);
                self.flush(Some(BlockKind::Rule));
            }
            "img" => {
                let source = attr_value(tag, "src").unwrap_or_default();
                if let Some(index) = self.images.get(source.as_str()) {
                    self.flush(None);
                    self.text = attr_value(tag, "alt").unwrap_or_default();
                    self.flush(Some(BlockKind::Image(*index)));
                }
            }
            "a" => {
                if let Some(href) = attr_value(tag, "href") {
                    self.open
                        .push((name.to_string(), Style::Link(href), self.text.len()));
                } else {
                    self.open.push((
                        name.to_string(),
                        Style::Link(String::new()),
                        self.text.len(),
                    ));
                }
            }
            _ => {
                if let Some((_, style)) = INLINE.iter().find(|(tag, _)| *tag == name) {
                    self.open
                        .push((name.to_string(), style.clone(), self.text.len()));
                }
            }
        }
    }

    fn close_tag(&mut self, name: &str) {
        if let Some(index) = self.open.iter().rposition(|(open, _, _)| open == name) {
            let (_, style, start) = self.open.remove(index);
            let end = self.text.len();
            if start < end && style != Style::Link(String::new()) {
                self.spans.push(Span { start, end, style });
            }
            return;
        }
        if name.len() == 2 && name.starts_with('h') && name != "hr" {
            self.flush(None);
            self.heading = None;
            return;
        }
        match name {
            "p" | "div" | "figure" | "figcaption" | "caption" | "dt" | "dd" | "table" => {
                self.flush(None)
            }
            "blockquote" => {
                self.flush(None);
                self.quotes = self.quotes.saturating_sub(1);
            }
            "pre" => {
                self.flush(None);
                self.pre = false;
            }
            "ul" | "ol" => {
                self.flush(None);
                self.lists.pop();
            }
            "li" => {
                self.flush(None);
                self.marker = None;
            }
            "tr" => {
                self.flush(None);
                self.table_row = false;
            }
            _ => {}
        }
    }

    fn run(&mut self, html: &str) {
        let mut at = 0;
        while at < html.len() {
            let Some(open) = html[at..].find('<') else {
                self.push_text(&html_escape::decode_html_entities(&html[at..]));
                break;
            };
            self.push_text(&html_escape::decode_html_entities(&html[at..at + open]));
            at += open;
            let Some(end) = find_tag_end(&html[at..]) else {
                break;
            };
            let tag = &html[at + 1..at + end];
            at += end + 1;
            let name = tag_name(tag);
            if tag.starts_with('/') {
                self.close_tag(&name);
            } else {
                self.open_tag(&name, tag);
                if tag.ends_with('/') {
                    self.close_tag(&name);
                }
            }
        }
        self.flush(None);
    }
}

impl Book {
    /// Lay out a document for reading.
    pub fn from_document(document: Document) -> Self {
        let names: Vec<String> = document
            .images
            .iter()
            .map(|image| format!("images/{}", image.name))
            .collect();
        let images: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.as_str(), index))
            .collect();
        let mut book = Book {
            title: document.metadata.title.clone(),
            ..Book::default()
        };
        for (number, section) in document.sections.iter().enumerate() {
            let mut layout = Layout {
                images: &images,
                blocks: Vec::new(),
                anchors: Vec::new(),
                text: String::new(),
                spans: Vec::new(),
                open: Vec::new(),
                heading: None,
                pre: false,
                table_row: false,
                quotes: 0,
                lists: Vec::new(),
                marker: None,
                pending_anchors: Vec::new(),
            };
            layout.run(&section.html);
            let last = layout.blocks.len().saturating_sub(1);
            for anchor in std::mem::take(&mut layout.pending_anchors) {
                layout.anchors.push((anchor, last));
            }
            for (anchor, block) in layout.anchors {
                book.anchors.entry(anchor).or_insert(Locator {
                    section: number,
                    block,
                    offset: 0,
                });
            }
            book.sections.push(ReadingSection {
                title: section.title.clone(),
                blocks: layout.blocks,
            });
        }
        let mut total = 0;
        for section in &book.sections {
            book.lengths.push(total);
            total += section
                .blocks
                .iter()
                .map(|block| block.text.len() + 1)
                .sum::<usize>();
        }
        book.lengths.push(total);

        // The book's own table of contents, or its headings.
        for item in &document.toc {
            if let Some(target) = book.anchors.get(&item.anchor) {
                book.toc.push(TocEntry {
                    title: item.title.clone(),
                    level: item.level.max(1),
                    target: *target,
                });
            }
        }
        if book.toc.is_empty() {
            for (number, section) in book.sections.iter().enumerate() {
                let mut found = false;
                for (index, block) in section.blocks.iter().enumerate() {
                    if let BlockKind::Heading(level @ 1..=3) = block.kind {
                        found = true;
                        book.toc.push(TocEntry {
                            title: block.text.replace('\n', " "),
                            level,
                            target: Locator {
                                section: number,
                                block: index,
                                offset: 0,
                            },
                        });
                    }
                }
                if !found {
                    book.toc.push(TocEntry {
                        title: if section.title.is_empty() {
                            format!("Section {}", number + 1)
                        } else {
                            section.title.clone()
                        },
                        level: 1,
                        target: Locator {
                            section: number,
                            ..Locator::default()
                        },
                    });
                }
            }
            let top = book.toc.iter().map(|entry| entry.level).min().unwrap_or(1);
            for entry in &mut book.toc {
                entry.level -= top - 1;
            }
        }
        book.images = document.images;
        book
    }

    /// Open a book file of any format that can be converted.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self::from_document(super::read_document(path, true)?))
    }

    pub fn block(&self, locator: Locator) -> Option<&Block> {
        self.sections
            .get(locator.section)?
            .blocks
            .get(locator.block)
    }

    /// Where a link inside the book leads; `None` for external addresses.
    pub fn resolve_link(&self, href: &str) -> Option<Locator> {
        self.anchors.get(href.strip_prefix('#')?).copied()
    }

    /// How far into the book a place is, from 0 to 1.
    pub fn fraction(&self, locator: Locator) -> f64 {
        let total = self.lengths.last().copied().unwrap_or(0);
        let Some(section) = self.sections.get(locator.section) else {
            return if total == 0 { 0.0 } else { 1.0 };
        };
        let before: usize = section
            .blocks
            .iter()
            .take(locator.block)
            .map(|block| block.text.len() + 1)
            .sum();
        let offset = section
            .blocks
            .get(locator.block)
            .map_or(0, |block| locator.offset.min(block.text.len()));
        if total == 0 {
            0.0
        } else {
            (self.lengths[locator.section] + before + offset) as f64 / total as f64
        }
    }

    /// The block at a fraction of the book.
    pub fn locate(&self, fraction: f64) -> Locator {
        let total = self.lengths.last().copied().unwrap_or(0);
        let target = (fraction.clamp(0.0, 1.0) * total as f64) as usize;
        let mut locator = Locator::default();
        for (number, section) in self.sections.iter().enumerate() {
            if self.lengths[number] > target {
                break;
            }
            let mut position = self.lengths[number];
            for (index, block) in section.blocks.iter().enumerate() {
                if position > target {
                    break;
                }
                locator = Locator {
                    section: number,
                    block: index,
                    offset: 0,
                };
                position += block.text.len() + 1;
            }
        }
        locator
    }

    /// The table-of-contents entry a place falls under.
    pub fn toc_index(&self, locator: Locator) -> Option<usize> {
        self.toc.iter().rposition(|entry| {
            entry.target
                <= Locator {
                    offset: usize::MAX,
                    ..locator
                }
        })
    }

    /// Find text, ignoring case; `limit` 0 returns every match.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let mut hits = Vec::new();
        let query = query.trim();
        if query.is_empty() {
            return hits;
        }
        let Ok(pattern) = RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()
        else {
            return hits;
        };
        for (number, section) in self.sections.iter().enumerate() {
            for (index, block) in section.blocks.iter().enumerate() {
                for found in pattern.find_iter(&block.text) {
                    hits.push(Hit {
                        start: Locator {
                            section: number,
                            block: index,
                            offset: found.start(),
                        },
                        end: found.end(),
                        snippet: snippet(&block.text, found.start(), found.end()),
                    });
                    if limit > 0 && hits.len() == limit {
                        return hits;
                    }
                }
            }
        }
        hits
    }

    /// Find where a passage is now. A passage saved at `near` is looked for
    /// there first and then in the closest block that contains it, so it
    /// survives edits to the book around it.
    pub fn relocate(&self, text: &str, near: Locator) -> Option<Locator> {
        if text.is_empty() {
            return self.block(near).map(|_| near);
        }
        if let Some(block) = self.block(near) {
            if block
                .text
                .get(near.offset..)
                .is_some_and(|rest| rest.starts_with(text))
            {
                return Some(near);
            }
        }
        let wanted = self.fraction(near);
        let mut best: Option<(f64, Locator)> = None;
        for (number, section) in self.sections.iter().enumerate() {
            for (index, block) in section.blocks.iter().enumerate() {
                for (offset, _) in block.text.match_indices(text) {
                    let locator = Locator {
                        section: number,
                        block: index,
                        offset,
                    };
                    let distance = (self.fraction(locator) - wanted).abs();
                    if best.is_none_or(|(least, _)| distance < least) {
                        best = Some((distance, locator));
                    }
                }
            }
        }
        best.map(|(_, locator)| locator)
    }
}

fn snippet(text: &str, start: usize, end: usize) -> String {
    const CONTEXT: usize = 40;
    let mut from = start.saturating_sub(CONTEXT);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (end + CONTEXT).min(text.len());
    while !text.is_char_boundary(to) {
        to += 1;
    }
    let mut snippet = String::new();
    if from > 0 {
        snippet.push('…');
    }
    snippet.push_str(&text[from..start]);
    snippet.push('[');
    snippet.push_str(&text[start..end]);
    snippet.push(']');
    snippet.push_str(&text[end..to]);
    if to < text.len() {
        snippet.push('…');
    }
    snippet.replace(['\n', '\t'], " ")
}
