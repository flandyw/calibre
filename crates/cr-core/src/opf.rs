//! OPF (Open Packaging Format) metadata reading and writing.
//!
//! `metadata.opf` files stored next to each book are calibre's authoritative
//! metadata backup, so writing a file calibre can read is part of staying
//! library-compatible.

use chrono::Utc;

use crate::error::{Error, Result};
use crate::metadata::{format_datetime, parse_datetime, Metadata};
use crate::text;
use crate::xml::{self, Node};

/// Parse OPF metadata. Cover bytes are not part of the OPF; format readers
/// resolve the cover item separately.
pub fn parse_opf(data: &[u8]) -> Result<Metadata> {
    let root = xml::parse(data)?;
    if root.name != "package" {
        return Err(Error::InvalidOpf(format!(
            "expected <package>, found <{}>",
            root.name
        )));
    }
    let metadata_node = root
        .child("metadata")
        .ok_or_else(|| Error::InvalidOpf("missing <metadata> element".into()))?;

    let mut meta = Metadata::default();
    meta.title = first_text(metadata_node, "title").unwrap_or_else(|| "Unknown".into());

    let creators: Vec<&Node> = metadata_node
        .children
        .iter()
        .filter(|c| c.name == "creator")
        .collect();
    for creator in &creators {
        let role = creator.attr_any(&["role", "opf:role"]);
        if matches!(role, Some(r) if !r.eq_ignore_ascii_case("aut")) {
            continue;
        }
        let name = creator.text_trimmed();
        if !name.is_empty() {
            meta.authors.push(name);
            if meta.author_sort.is_none() {
                if let Some(file_as) = creator.attr_any(&["file-as", "opf:file-as"]) {
                    if !file_as.trim().is_empty() {
                        meta.author_sort = Some(file_as.trim().to_string());
                    }
                }
            }
        }
    }
    if meta.authors.is_empty() {
        // Some OPFs mark creators with a non-aut role or omit the role.
        for creator in &creators {
            let name = creator.text_trimmed();
            if !name.is_empty() {
                meta.authors.push(name);
            }
        }
    }

    meta.languages = metadata_node
        .children
        .iter()
        .filter(|c| c.name == "language")
        .map(|c| c.text_trimmed())
        .filter(|s| !s.is_empty())
        .collect();
    meta.publisher = first_text(metadata_node, "publisher");
    meta.comments = metadata_node
        .child("description")
        .map(|n| n.text.trim().to_string())
        .filter(|s| !s.is_empty());
    meta.tags = metadata_node
        .children
        .iter()
        .filter(|c| c.name == "subject")
        .map(|c| c.text_trimmed())
        .filter(|s| !s.is_empty())
        .collect();

    if let Some(date) = first_text(metadata_node, "date") {
        meta.pubdate = parse_datetime(&date);
    }

    // Identifiers and the package UUID.
    for identifier in metadata_node
        .children
        .iter()
        .filter(|c| c.name == "identifier")
    {
        let value = identifier.text_trimmed();
        if value.is_empty() {
            continue;
        }
        let scheme = identifier
            .attr_any(&["scheme", "opf:scheme"])
            .map(|s| s.to_ascii_lowercase());
        let id = identifier
            .attr("id")
            .unwrap_or_default()
            .to_ascii_lowercase();
        match scheme.as_deref() {
            Some("uuid") => meta.uuid = Some(value),
            Some("isbn") => {
                meta.identifiers.insert("isbn".into(), value);
            }
            // calibre's own application id is not user-facing metadata.
            Some("calibre") => {}
            Some(other) => {
                meta.identifiers.insert(other.to_string(), value);
            }
            None => {
                if id == "uuid_id" {
                    meta.uuid = Some(value);
                } else if id.contains("calibre") {
                    // calibre's application id; not user-facing metadata.
                } else if !meta.identifiers.contains_key("uuid") {
                    meta.identifiers.insert("uuid".into(), value);
                }
            }
        }
    }
    if meta.uuid.is_none() {
        // `<package unique-identifier="uuid_id">` points at the identifier.
        if let Some(unique_id) = root.attr("unique-identifier") {
            if let Some(node) = metadata_node
                .children
                .iter()
                .find(|c| c.name == "identifier" && c.attr("id") == Some(unique_id))
            {
                meta.uuid = Some(node.text_trimmed());
            }
        }
    }

    // calibre-specific <meta name="..." content="..."> records.
    for meta_node in metadata_node.children.iter().filter(|c| c.name == "meta") {
        let name = meta_node.attr("name").unwrap_or_default();
        let content = meta_node
            .attr("content")
            .map(str::to_string)
            .or_else(|| {
                if meta_node.attr("property").is_some() {
                    Some(meta_node.text_trimmed())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        match name {
            "calibre:series" => {
                if !content.trim().is_empty() {
                    meta.series = Some(content.trim().to_string());
                }
            }
            "calibre:series_index" => {
                if let Ok(index) = content.trim().parse::<f64>() {
                    meta.series_index = index;
                }
            }
            "calibre:rating" => {
                if let Ok(rating) = content.trim().parse::<u8>() {
                    if (1..=10).contains(&rating) {
                        meta.rating = Some(rating);
                    }
                }
            }
            "calibre:timestamp" => meta.timestamp = parse_datetime(&content),
            "calibre:title_sort" => {
                if !content.trim().is_empty() {
                    meta.title_sort = Some(content.trim().to_string());
                }
            }
            "calibre:author_link_map" => {}
            _ => {}
        }
    }

    meta.normalize();
    Ok(meta)
}

/// Render a calibre-compatible `metadata.opf`.
pub fn metadata_to_opf(meta: &Metadata) -> String {
    let uuid = meta
        .uuid
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut out = String::with_capacity(2048);
    out.push_str("<?xml version='1.0' encoding='utf-8'?>\n");
    out.push_str(
        "<package xmlns=\"http://www.idpf.org/2007/opf\" unique-identifier=\"uuid_id\" version=\"2.0\">\n",
    );
    out.push_str("  <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:opf=\"http://www.idpf.org/2007/opf\">\n");
    out.push_str(&format!(
        "    <dc:identifier opf:scheme=\"calibre\" id=\"calibre_id\">{}</dc:identifier>\n",
        escape(&uuid)
    ));
    out.push_str(&format!(
        "    <dc:identifier opf:scheme=\"uuid\" id=\"uuid_id\">{}</dc:identifier>\n",
        escape(&uuid)
    ));
    out.push_str(&format!(
        "    <dc:title>{}</dc:title>\n",
        escape(&meta.title)
    ));
    if let Some(sort) = &meta.title_sort {
        out.push_str(&format!(
            "    <meta name=\"calibre:title_sort\" content=\"{}\"/>\n",
            escape_attr(sort)
        ));
    }
    for author in &meta.authors {
        out.push_str(&format!(
            "    <dc:creator opf:file-as=\"{}\" opf:role=\"aut\">{}</dc:creator>\n",
            escape_attr(&text::author_sort(author)),
            escape(author)
        ));
    }
    for language in &meta.languages {
        out.push_str(&format!(
            "    <dc:language>{}</dc:language>\n",
            escape(language)
        ));
    }
    if let Some(publisher) = &meta.publisher {
        out.push_str(&format!(
            "    <dc:publisher>{}</dc:publisher>\n",
            escape(publisher)
        ));
    }
    if let Some(pubdate) = &meta.pubdate {
        out.push_str(&format!(
            "    <dc:date>{}</dc:date>\n",
            pubdate.format("%Y-%m-%dT%H:%M:%S%:z")
        ));
    }
    if let Some(comments) = &meta.comments {
        out.push_str(&format!(
            "    <dc:description>{}</dc:description>\n",
            escape(comments)
        ));
    }
    for tag in &meta.tags {
        out.push_str(&format!("    <dc:subject>{}</dc:subject>\n", escape(tag)));
    }
    for (scheme, value) in &meta.identifiers {
        out.push_str(&format!(
            "    <dc:identifier opf:scheme=\"{}\">{}</dc:identifier>\n",
            escape_attr(&scheme.to_ascii_uppercase()),
            escape(value)
        ));
    }
    if let Some(series) = &meta.series {
        out.push_str(&format!(
            "    <meta name=\"calibre:series\" content=\"{}\"/>\n",
            escape_attr(series)
        ));
        out.push_str(&format!(
            "    <meta name=\"calibre:series_index\" content=\"{}\"/>\n",
            text::format_series_index(meta.series_index)
        ));
    }
    if let Some(rating) = meta.rating {
        out.push_str(&format!(
            "    <meta name=\"calibre:rating\" content=\"{rating}\"/>\n"
        ));
    }
    let timestamp = meta.timestamp.unwrap_or_else(Utc::now);
    out.push_str(&format!(
        "    <meta name=\"calibre:timestamp\" content=\"{}\"/>\n",
        escape_attr(&format_datetime(&timestamp))
    ));
    out.push_str("  </metadata>\n  <guide/>\n</package>\n");
    out
}

fn first_text(parent: &Node, name: &str) -> Option<String> {
    parent
        .children
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.text_trimmed())
        .filter(|s| !s.is_empty())
}

fn escape(input: &str) -> String {
    html_escape::encode_text(input).to_string()
}

fn escape_attr(input: &str) -> String {
    html_escape::encode_double_quoted_attribute(input).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="uuid_id" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:identifier opf:scheme="calibre" id="calibre_id">11111111-1111-1111-1111-111111111111</dc:identifier>
    <dc:identifier opf:scheme="uuid" id="uuid_id">22222222-2222-2222-2222-222222222222</dc:identifier>
    <dc:identifier opf:scheme="ISBN">9780000000000</dc:identifier>
    <dc:title>The Hobbit</dc:title>
    <dc:creator opf:file-as="Tolkien, J. R. R." opf:role="aut">J. R. R. Tolkien</dc:creator>
    <dc:language>en</dc:language>
    <dc:publisher>Allen &amp; Unwin</dc:publisher>
    <dc:date>1937-09-21T00:00:00+00:00</dc:date>
    <dc:description>&lt;p&gt;A hobbit goes there and back again.&lt;/p&gt;</dc:description>
    <dc:subject>Fantasy</dc:subject>
    <dc:subject>Classic</dc:subject>
    <meta name="calibre:series" content="Middle Earth"/>
    <meta name="calibre:series_index" content="0.5"/>
    <meta name="calibre:rating" content="10"/>
    <meta name="calibre:timestamp" content="2024-01-02 03:04:05+00:00"/>
  </metadata>
  <guide/>
</package>"#;

    #[test]
    fn parses_opf_metadata() {
        let meta = parse_opf(SAMPLE.as_bytes()).unwrap();
        assert_eq!(meta.title, "The Hobbit");
        assert_eq!(meta.authors, vec!["J. R. R. Tolkien"]);
        assert_eq!(meta.publisher.as_deref(), Some("Allen & Unwin"));
        assert_eq!(meta.series.as_deref(), Some("Middle Earth"));
        assert_eq!(meta.series_index, 0.5);
        assert_eq!(meta.rating, Some(10));
        assert_eq!(meta.tags, vec!["Fantasy", "Classic"]);
        assert_eq!(meta.languages, vec!["en"]);
        assert_eq!(
            meta.identifiers.get("isbn").map(String::as_str),
            Some("9780000000000")
        );
        assert_eq!(
            meta.uuid.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
        assert!(meta.comments.as_deref().unwrap().contains("hobbit"));
    }

    #[test]
    fn writes_opf_that_parses_back() {
        let meta = parse_opf(SAMPLE.as_bytes()).unwrap();
        let written = metadata_to_opf(&meta);
        let round_tripped = parse_opf(written.as_bytes()).unwrap();
        assert_eq!(round_tripped.title, meta.title);
        assert_eq!(round_tripped.authors, meta.authors);
        assert_eq!(round_tripped.publisher, meta.publisher);
        assert_eq!(round_tripped.series, meta.series);
        assert_eq!(round_tripped.series_index, meta.series_index);
        assert_eq!(round_tripped.rating, meta.rating);
        assert_eq!(round_tripped.tags, meta.tags);
        assert_eq!(round_tripped.identifiers, meta.identifiers);
        assert_eq!(round_tripped.uuid, meta.uuid);
    }
}
