//! A tiny, namespace-insensitive XML tree.
//!
//! Ebook metadata files are small; pulling in a full DOM is unnecessary. This
//! parser keeps element text and children, ignores namespace prefixes (so
//! `dc:title`, `opf:role` and `l:href` are addressable as `title`, `role` and
//! `href`) and is shared by the OPF, EPUB container and FB2 readers.

use crate::error::{Error, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// A parsed XML element.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    /// Text directly inside this element (not descendants).
    pub text: String,
}

impl Node {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// First direct child with the given local name.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// First descendant (depth first) with the given local name.
    pub fn descendant(&self, name: &str) -> Option<&Node> {
        for child in &self.children {
            if child.name == name {
                return Some(child);
            }
            if let Some(found) = child.descendant(name) {
                return Some(found);
            }
        }
        None
    }

    /// All descendants (depth first) with the given local name.
    pub fn descendants<'a>(&'a self, name: &'a str) -> Vec<&'a Node> {
        let mut out = Vec::new();
        self.collect_descendants(name, &mut out);
        out
    }

    fn collect_descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        for child in &self.children {
            if child.name == name {
                out.push(child);
            }
            child.collect_descendants(name, out);
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Attribute lookup that also accepts a namespace-qualified spelling.
    pub fn attr_any(&self, names: &[&str]) -> Option<&str> {
        for name in names {
            if let Some(value) = self.attr(name) {
                return Some(value);
            }
        }
        None
    }

    /// Trimmed text directly inside this node.
    pub fn text_trimmed(&self) -> String {
        self.text.trim().to_string()
    }

    /// All text under this node, in document order, whitespace normalised.
    pub fn deep_text(&self) -> String {
        let mut out = String::new();
        self.append_text(&mut out);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn append_text(&self, out: &mut String) {
        if !self.text.is_empty() {
            out.push_str(&self.text);
            out.push(' ');
        }
        for child in &self.children {
            child.append_text(out);
        }
    }
}

/// Parse XML bytes into a tree. Encoding declarations are honoured.
pub fn parse(data: &[u8]) -> Result<Node> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut stack = vec![Node::new("#document")];

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                stack.push(node_from_start(&e)?);
            }
            Ok(Event::Empty(e)) => {
                let node = node_from_start(&e)?;
                stack
                    .last_mut()
                    .expect("root always present")
                    .children
                    .push(node);
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().map_err(|err| Error::Xml(err.to_string()))?;
                if let Some(parent) = stack.last_mut() {
                    parent.text.push_str(&text);
                }
            }
            Ok(Event::CData(e)) => {
                if let Some(parent) = stack.last_mut() {
                    parent.text.push_str(&String::from_utf8_lossy(&e));
                }
            }
            Ok(Event::End(_)) => {
                if stack.len() > 1 {
                    let node = stack.pop().expect("stack is non-empty");
                    stack
                        .last_mut()
                        .expect("root always present")
                        .children
                        .push(node);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => return Err(Error::Xml(err.to_string())),
        }
        buf.clear();
    }

    let document = stack.remove(0);
    if document.name == "#document" {
        document
            .children
            .into_iter()
            .next()
            .ok_or_else(|| Error::Xml("XML document has no root element".into()))
    } else {
        Ok(document)
    }
}

fn node_from_start(e: &quick_xml::events::BytesStart<'_>) -> Result<Node> {
    let name = local_name(e.name().as_ref()).to_string();
    let mut attrs = Vec::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| Error::Xml(err.to_string()))?;
        let key = local_name(attr.key.as_ref()).to_string();
        let raw = String::from_utf8_lossy(&attr.value);
        let value = html_escape::decode_html_entities(&raw).to_string();
        attrs.push((key, value));
    }
    Ok(Node {
        name,
        attrs,
        ..Default::default()
    })
}

fn local_name(raw: &[u8]) -> &str {
    let name = std::str::from_utf8(raw).unwrap_or_default();
    name.rsplit(':').next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_elements_and_namespaces() {
        let xml = br#"<?xml version="1.0"?>
            <package xmlns="http://www.idpf.org/2007/opf">
              <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                <dc:title id="t">The &amp; Hobbit</dc:title>
                <dc:creator opf:role="aut">J. R. R. Tolkien</dc:creator>
              </metadata>
            </package>"#;
        let root = parse(xml).unwrap();
        assert_eq!(root.name, "package");
        let metadata = root.child("metadata").unwrap();
        assert_eq!(
            metadata.child("title").unwrap().text_trimmed(),
            "The & Hobbit"
        );
        let creator = metadata.child("creator").unwrap();
        assert_eq!(creator.attr("role"), Some("aut"));
        assert_eq!(creator.text_trimmed(), "J. R. R. Tolkien");
    }

    #[test]
    fn handles_empty_elements_and_cdata() {
        let xml = br#"<root><item id="x"/><body><![CDATA[a < b]]></body></root>"#;
        let root = parse(xml).unwrap();
        assert_eq!(root.child("item").unwrap().attr("id"), Some("x"));
        assert_eq!(root.child("body").unwrap().text, "a < b");
    }
}
