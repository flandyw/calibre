//! FictionBook 2 (FB2) support.

use std::path::Path;

use base64::Engine;
use chrono::{NaiveDate, TimeZone, Utc};

use crate::error::{Error, Result};
use crate::formats::{BookText, Chapter};
use crate::metadata::{htmlize_comments, Metadata};
use crate::xml::{self, Node};

pub fn read(path: &Path) -> Result<(Metadata, Option<Vec<u8>>)> {
    let data = std::fs::read(path)?;
    let root = xml::parse(&data)?;
    if root.name != "FictionBook" {
        return Err(Error::InvalidMetadata(format!(
            "not an FB2 document (root is <{}>)",
            root.name
        )));
    }

    let description = root.child("description");
    let title_info = description.and_then(|d| d.child("title-info"));
    let mut meta = Metadata::default();
    meta.format = Some("FB2".to_string());

    if let Some(title_info) = title_info {
        meta.title = title_info
            .child("book-title")
            .map(|n| n.deep_text())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| super::txt::title_from_path(path));

        for author in title_info.children.iter().filter(|c| c.name == "author") {
            let mut parts = Vec::new();
            for field in ["first-name", "middle-name", "last-name"] {
                if let Some(value) = author.child(field).map(|n| n.deep_text()) {
                    if !value.is_empty() {
                        parts.push(value);
                    }
                }
            }
            let name = if parts.is_empty() {
                author
                    .child("nickname")
                    .map(|n| n.deep_text())
                    .unwrap_or_default()
            } else {
                parts.join(" ")
            };
            if !name.is_empty() {
                meta.authors.push(name);
            }
        }

        meta.tags = title_info
            .children
            .iter()
            .filter(|c| c.name == "genre")
            .map(|n| n.deep_text())
            .filter(|s| !s.is_empty())
            .collect();

        if let Some(lang) = title_info.child("lang").map(|n| n.deep_text()) {
            if !lang.is_empty() {
                meta.languages.push(lang);
            }
        }
        if let Some(annotation) = title_info.child("annotation") {
            let text = annotation.deep_text();
            if !text.is_empty() {
                meta.comments = Some(htmlize_comments(&text));
            }
        }
        if let Some(sequence) = title_info.child("sequence") {
            if let Some(name) = sequence.attr("name") {
                if !name.trim().is_empty() {
                    meta.series = Some(name.trim().to_string());
                }
            }
            if let Some(number) = sequence.attr("number") {
                if let Ok(index) = number.trim().parse::<f64>() {
                    if index > 0.0 {
                        meta.series_index = index;
                    }
                }
            }
        }
    } else {
        meta.title = super::txt::title_from_path(path);
    }

    if let Some(publish_info) = description.and_then(|d| d.child("publish-info")) {
        if let Some(publisher) = publish_info.child("publisher").map(|n| n.deep_text()) {
            if !publisher.is_empty() {
                meta.publisher = Some(publisher);
            }
        }
        if let Some(year) = publish_info.child("year").map(|n| n.deep_text()) {
            if let Ok(year) = year.parse::<i32>() {
                if let Some(date) = NaiveDate::from_ymd_opt(year, 1, 1) {
                    meta.pubdate = Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap()));
                }
            }
        }
    }

    if let Some(document_info) = description.and_then(|d| d.child("document-info")) {
        for field in ["id", "isbn"] {
            if let Some(value) = document_info.child(field).map(|n| n.deep_text()) {
                if !value.is_empty() {
                    meta.identifiers.insert(field.to_string(), value);
                }
            }
        }
        if let Some(date) = document_info.child("date").map(|n| n.deep_text()) {
            meta.timestamp = crate::metadata::parse_datetime(&date);
        }
    }

    let cover = extract_cover(&root, title_info);
    meta.normalize();
    Ok((meta, cover))
}

pub fn text(path: &Path) -> Result<BookText> {
    let data = std::fs::read(path)?;
    let root = xml::parse(&data)?;
    let body = root
        .child("body")
        .ok_or_else(|| Error::InvalidMetadata("FB2 has no <body>".into()))?;

    let mut chapters = Vec::new();
    let sections: Vec<&Node> = body
        .children
        .iter()
        .filter(|c| c.name == "section")
        .collect();

    if sections.is_empty() {
        let text = collect_text(body);
        if !text.trim().is_empty() {
            chapters.push(Chapter {
                title: root
                    .child("description")
                    .and_then(|d| d.child("title-info"))
                    .and_then(|t| t.child("book-title"))
                    .map(|n| n.deep_text())
                    .unwrap_or_else(|| "Text".to_string()),
                text,
            });
        }
    } else {
        for (index, section) in sections.iter().enumerate() {
            let title = section
                .child("title")
                .map(|t| t.deep_text())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("Chapter {}", index + 1));
            let text = collect_text(section);
            if !text.trim().is_empty() {
                chapters.push(Chapter { title, text });
            }
        }
    }

    Ok(BookText { chapters })
}

fn extract_cover(root: &Node, title_info: Option<&Node>) -> Option<Vec<u8>> {
    let href = title_info?
        .child("coverpage")?
        .child("image")?
        .attr_any(&["href", "l:href"])?;
    let id = href.trim_start_matches('#');
    let binary = root
        .children
        .iter()
        .filter(|c| c.name == "binary")
        .find(|c| c.attr("id") == Some(id))?;
    let encoded: String = binary.text.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

fn collect_text(node: &Node) -> String {
    let mut out = String::new();
    collect_into(node, &mut out);
    normalize(&out)
}

fn collect_into(node: &Node, out: &mut String) {
    for child in &node.children {
        match child.name.as_str() {
            "title" => {
                let text = child.deep_text();
                if !text.is_empty() {
                    out.push_str(&text);
                    out.push_str("\n\n");
                }
            }
            "p" | "subtitle" | "v" | "text-author" | "date" | "epigraph" => {
                let text = child.deep_text();
                if !text.is_empty() {
                    out.push_str(&text);
                    out.push_str("\n\n");
                }
            }
            "empty-line" => out.push('\n'),
            // Skip binary data and cover images while reading.
            "binary" | "image" => {}
            _ => collect_into(child, out),
        }
    }
}

fn normalize(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in input.replace('\r', "\n").split('\n') {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(collapsed);
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}
