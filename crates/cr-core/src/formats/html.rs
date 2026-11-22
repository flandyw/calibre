//! Minimal HTML/XHTML handling: enough to read EPUB chapters and HTML files
//! without a full browser engine.

use super::Chapter;

pub(super) fn chapter(markup: &str, number: usize) -> Option<Chapter> {
    let body = html_to_text(markup);
    if body.trim().is_empty() {
        return None;
    }
    let title = first_heading(markup)
        .or_else(|| document_title(markup))
        .unwrap_or_else(|| format!("Chapter {number}"));
    let mut lines = body.lines();
    let text = if lines
        .next()
        .is_some_and(|first| first.trim().eq_ignore_ascii_case(title.trim()))
    {
        lines.collect::<Vec<_>>().join("\n").trim().to_string()
    } else {
        body
    };
    Some(Chapter { title, text })
}

/// Tags that start a new paragraph when converting HTML to text.
const BLOCK_TAGS: [&str; 26] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
];

/// Convert HTML to readable plain text, preserving paragraph breaks.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut raw = String::with_capacity(html.len());
    let mut pos = 0usize;

    while pos < html.len() {
        let Some(rel_lt) = html[pos..].find('<') else {
            raw.push_str(&html[pos..]);
            break;
        };
        let lt = pos + rel_lt;
        raw.push_str(&html[pos..lt]);

        if html[lt..].starts_with("<!--") {
            pos = html[lt + 4..]
                .find("-->")
                .map(|end| lt + 4 + end + 3)
                .unwrap_or(html.len());
            continue;
        }

        let Some(rel_end) = find_tag_end(&html[lt + 1..]) else {
            // Unterminated tag: treat the rest as text.
            raw.push_str(&html[lt..]);
            break;
        };
        let end = lt + 1 + rel_end;
        let tag = &html[lt + 1..end];
        pos = end + 1;

        let closing = tag.starts_with('/');
        let name = tag_name(tag);
        if name.is_empty() {
            continue;
        }

        // Elements whose entire content is not prose.
        if matches!(
            name.as_str(),
            "script" | "style" | "head" | "title" | "noscript" | "template"
        ) {
            if !closing {
                let closing_tag = format!("</{name}");
                if let Some(idx) = find_ignore_case(&lower, &closing_tag, pos) {
                    if let Some(rel) = find_tag_end(&html[idx + 1..]) {
                        pos = idx + 1 + rel + 1;
                    } else {
                        pos = html.len();
                    }
                }
            }
            continue;
        }

        if name == "img" && !closing {
            if let Some(alt) = attr_value(tag, "alt") {
                if !alt.trim().is_empty() {
                    raw.push_str(alt.trim());
                    raw.push(' ');
                }
            }
            continue;
        }

        if closing {
            if BLOCK_TAGS.contains(&name.as_str())
                || matches!(name.as_str(), "li" | "tr" | "ul" | "td" | "th")
            {
                raw.push_str("\n\n");
            }
            continue;
        }

        if name == "br" {
            raw.push('\n');
        } else if BLOCK_TAGS.contains(&name.as_str()) {
            raw.push_str("\n\n");
        } else if matches!(name.as_str(), "li" | "tr") {
            raw.push('\n');
        } else if name == "td" || name == "th" {
            raw.push_str("  ");
        }
    }

    normalize_text(&raw)
}

/// Text of the first h1/h2/h3 element, used as a chapter title.
pub fn first_heading(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    for level in 1..=3 {
        let open = format!("<h{level}");
        let mut search_from = 0usize;
        while let Some(idx) = find_ignore_case(&lower, &open, search_from) {
            let after = idx + open.len();
            // Ensure the tag name ended (avoid matching <h10>).
            let next = lower[after..].chars().next();
            if matches!(next, Some(c) if c.is_ascii_alphanumeric()) {
                search_from = after;
                continue;
            }
            let Some(rel_end) = find_tag_end(&html[after..]) else {
                return None;
            };
            let content_start = after + rel_end + 1;
            let close = format!("</h{level}");
            let Some(close_idx) = find_ignore_case(&lower, &close, content_start) else {
                return None;
            };
            let text = html_to_text(&html[content_start..close_idx]);
            if !text.trim().is_empty() {
                return Some(text.trim().to_string());
            }
            search_from = close_idx + close.len();
        }
    }
    None
}

/// Value of the document `<title>` element.
pub fn document_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = find_ignore_case(&lower, "<title", 0)?;
    let Some(rel_end) = find_tag_end(&html[open + 1..]) else {
        return None;
    };
    let start = open + 1 + rel_end + 1;
    let end = find_ignore_case(&lower, "</title", start)?;
    let text = html_to_text(&html[start..end]);
    if text.trim().is_empty() {
        None
    } else {
        Some(text.trim().to_string())
    }
}

/// Read a `<meta name="..." content="...">` value (also supports `property`).
pub fn meta_content(html: &str, names: &[&str]) -> Option<String> {
    let mut pos = 0usize;
    while let Some(rel_lt) = html[pos..].find('<') {
        let lt = pos + rel_lt;
        let Some(rel_end) = find_tag_end(&html[lt + 1..]) else {
            break;
        };
        let end = lt + 1 + rel_end;
        let tag = &html[lt + 1..end];
        pos = end + 1;
        if tag_name(tag) != "meta" {
            continue;
        }
        let key = attr_value(tag, "name").or_else(|| attr_value(tag, "property"));
        let Some(key) = key else { continue };
        if names.iter().any(|n| key.eq_ignore_ascii_case(n)) {
            if let Some(content) = attr_value(tag, "content") {
                let content = content.trim().to_string();
                if !content.is_empty() {
                    return Some(content);
                }
            }
        }
    }
    None
}

/// Collapse whitespace in an HTML fragment and strip all tags.
pub fn strip_tags(html: &str) -> String {
    html_to_text(html)
}

fn normalize_text(raw: &str) -> String {
    let decoded = html_escape::decode_html_entities(raw).to_string();
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.replace('\r', "\n").split('\n') {
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

pub(super) fn tag_name(tag: &str) -> String {
    let tag = tag.trim_start_matches('/').trim_start();
    tag.split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub(super) fn attr_value(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut i = 0usize;
    // Skip the tag name.
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
            && bytes[i] != b'>'
        {
            i += 1;
        }
        let key = &tag[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let quote = bytes[i];
        let value;
        if quote == b'"' || quote == b'\'' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            value = &tag[start..i];
            if i < bytes.len() {
                i += 1;
            }
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            value = &tag[start..i];
        }
        if key.eq_ignore_ascii_case(name) {
            return Some(html_escape::decode_html_entities(value).to_string());
        }
    }
    None
}

pub(super) fn find_tag_end(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

fn find_ignore_case(haystack_lower: &str, needle: &str, from: usize) -> Option<usize> {
    haystack_lower
        .get(from..)?
        .find(&needle.to_ascii_lowercase())
        .map(|idx| idx + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_html_to_text() {
        let html = r#"<html><head><title>T</title><style>p {color:red}</style></head>
            <body><h1>Chapter One</h1><p>Hello <em>world</em>.</p>
            <p>Second &amp; last.</p><ul><li>One</li><li>Two</li></ul>
            <script>alert('no')</script></body></html>"#;
        let text = html_to_text(html);
        assert!(text.starts_with("Chapter One"));
        assert!(text.contains("Hello world."));
        assert!(text.contains("Second & last."));
        assert!(text.contains("One"));
        assert!(text.contains("Two"));
        assert!(!text.contains("color:red"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("T\n"));
    }

    #[test]
    fn extracts_heading_and_title() {
        let html = "<html><head><title>Book</title></head><body><h2>Chapter 3</h2></body></html>";
        assert_eq!(document_title(html).as_deref(), Some("Book"));
        assert_eq!(first_heading(html).as_deref(), Some("Chapter 3"));
    }

    #[test]
    fn reads_meta_tags() {
        let html = r#"<meta name="author" content="Jane &amp; Joe"><meta property="og:title" content="X">"#;
        assert_eq!(
            meta_content(html, &["author"]).as_deref(),
            Some("Jane & Joe")
        );
        assert_eq!(meta_content(html, &["og:title"]).as_deref(), Some("X"));
    }

    #[test]
    fn handles_unquoted_attributes_and_entities() {
        let html = "<p class=x>a&nbsp;b &#8212; c</p>";
        assert_eq!(html_to_text(html), "a b \u{2014} c");
    }
}
