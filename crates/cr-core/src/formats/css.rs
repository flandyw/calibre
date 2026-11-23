//! The part of CSS a reflowed book needs: which fonts a stylesheet embeds,
//! and for each element its alignment, size, weight, slant, decoration,
//! margins and visibility.
//!
//! Selectors made of element names, classes and ids, joined by descendant
//! or child combinators, are understood; a rule with any other selector is
//! ignored, as a browser ignores what it does not know.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vertical {
    Super,
    Sub,
}

/// An `@font-face` rule that names a file of the book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFace {
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    /// As written in `src`, e.g. `fonts/font1.ttf`.
    pub source: String,
}

/// The style of an element after the cascade. The first group is inherited
/// by the elements inside it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Computed {
    pub align: Option<Align>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    /// Relative to the reader's text size.
    pub size: Option<f32>,
    pub family: Option<String>,
    /// First-line indent in em.
    pub indent: Option<f32>,

    pub hidden: bool,
    pub underline: bool,
    pub strike: bool,
    pub vertical: Option<Vertical>,
    /// In em.
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
}

impl Computed {
    fn inherit(parent: &Computed) -> Self {
        Computed {
            align: parent.align,
            bold: parent.bold,
            italic: parent.italic,
            size: parent.size,
            family: parent.family.clone(),
            indent: parent.indent,
            ..Computed::default()
        }
    }
}

/// An open element, as selectors see it.
#[derive(Debug, Clone, Default)]
pub struct Element {
    pub tag: String,
    pub id: String,
    pub classes: Vec<String>,
}

#[derive(Debug, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl Compound {
    fn matches(&self, element: &Element) -> bool {
        self.tag.as_ref().is_none_or(|tag| *tag == element.tag)
            && self.id.as_ref().is_none_or(|id| *id == element.id)
            && self
                .classes
                .iter()
                .all(|class| element.classes.contains(class))
    }
}

#[derive(Debug)]
struct Rule {
    /// Outermost first; the last part is the element itself.
    selector: Vec<Compound>,
    specificity: u32,
    declarations: usize,
}

#[derive(Debug, Default)]
pub struct Stylesheet {
    rules: Vec<Rule>,
    declarations: Vec<Vec<(String, String)>>,
    /// Rule numbers by the tag, `.class` or `#id` their last part needs.
    index: HashMap<String, Vec<usize>>,
    pub faces: Vec<FontFace>,
}

static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/").unwrap());
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^)'"]*))\s*\)"#).unwrap());
static IMPORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)@import[^;]*;").unwrap());

/// Replace the target of every `url()`; `None` leaves it pointing nowhere.
/// `@import` rules are dropped: the files they name are not followed.
pub fn rewrite_urls(css: &str, map: &mut dyn FnMut(&str) -> Option<String>) -> String {
    let css = IMPORT.replace_all(css, "");
    URL.replace_all(&css, |found: &regex::Captures<'_>| {
        let target = (1..=3)
            .find_map(|group| found.get(group))
            .map_or("", |target| target.as_str());
        let mapped = map(target.trim()).unwrap_or_default();
        format!("url(\"{}\")", mapped.replace(['"', '\\', '\n'], ""))
    })
    .into_owned()
}

/// The end of the block that opens at `open`, a `{`.
fn block_end(css: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (offset, byte) in css.bytes().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return offset;
                }
            }
            _ => {}
        }
    }
    css.len()
}

fn declarations(block: &str) -> Vec<(String, String)> {
    block
        .split(';')
        .filter_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            let value = value.trim().to_ascii_lowercase();
            let value = value.strip_suffix("!important").unwrap_or(&value).trim();
            Some((name.trim().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn selector(text: &str) -> Option<Vec<Compound>> {
    let mut parts = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == '>') {
        if word.is_empty() {
            continue;
        }
        let mut compound = Compound::default();
        let mut rest = word;
        let name_end = |text: &str| {
            text.find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(text.len())
        };
        let end = name_end(rest);
        if end > 0 {
            compound.tag = Some(rest[..end].to_ascii_lowercase());
            rest = &rest[end..];
        } else if let Some(after) = rest.strip_prefix('*') {
            rest = after;
        }
        while !rest.is_empty() {
            let kind = rest.as_bytes()[0];
            let end = name_end(&rest[1..]);
            if end == 0 {
                return None;
            }
            let name = rest[1..=end].to_string();
            match kind {
                b'.' => compound.classes.push(name),
                b'#' => compound.id = Some(name),
                _ => return None,
            }
            rest = &rest[end + 1..];
        }
        parts.push(compound);
    }
    (!parts.is_empty()).then_some(parts)
}

impl Stylesheet {
    pub fn parse(css: &str) -> Self {
        let mut sheet = Stylesheet::default();
        let css = COMMENT.replace_all(css, " ");
        sheet.read(&css, 0);
        sheet
    }

    fn read(&mut self, css: &str, depth: u8) {
        let mut at = 0;
        while at < css.len() {
            let Some(open) = css[at..].find('{').map(|open| open + at) else {
                break;
            };
            // A statement that ends before the block, such as `@charset`.
            if let Some(end) = css[at..open].rfind(';') {
                at += end + 1;
            }
            let prelude = css[at..open].trim();
            let close = block_end(css, open);
            let block = &css[open + 1..close];
            at = (close + 1).min(css.len());
            if let Some(rule) = prelude.strip_prefix('@') {
                let lower = rule.to_ascii_lowercase();
                if lower.starts_with("font-face") {
                    self.face(block);
                } else if lower.starts_with("media") && depth < 4 {
                    let query = lower["media".len()..].trim();
                    let screen = query.is_empty()
                        || query
                            .split(',')
                            .any(|one| !one.contains("print") && !one.contains("speech"));
                    if screen {
                        self.read(block, depth + 1);
                    }
                }
                continue;
            }
            let declared = declarations(block);
            if declared.is_empty() {
                continue;
            }
            let number = self.declarations.len();
            self.declarations.push(declared);
            for one in prelude.split(',') {
                let Some(selector) = selector(one.trim()) else {
                    continue;
                };
                let specificity = selector
                    .iter()
                    .map(|part| {
                        u32::from(part.id.is_some()) * 10_000
                            + part.classes.len() as u32 * 100
                            + u32::from(part.tag.is_some())
                    })
                    .sum();
                let last = selector.last().expect("a selector has a part");
                let key = match (&last.id, last.classes.first(), &last.tag) {
                    (Some(id), _, _) => format!("#{id}"),
                    (None, Some(class), _) => format!(".{class}"),
                    (None, None, Some(tag)) => tag.clone(),
                    (None, None, None) => "*".to_string(),
                };
                self.index.entry(key).or_default().push(self.rules.len());
                self.rules.push(Rule {
                    selector,
                    specificity,
                    declarations: number,
                });
            }
        }
    }

    fn face(&mut self, block: &str) {
        // `src` holds URLs, whose case matters.
        let source = block.split(';').find_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            (name.trim().eq_ignore_ascii_case("src")).then_some(value)
        });
        let Some(source) = source.and_then(|source| {
            URL.captures_iter(source)
                .filter_map(|found| (1..=3).find_map(|group| found.get(group)))
                .map(|target| target.as_str().trim().to_string())
                .find(|target| !target.is_empty())
        }) else {
            return;
        };
        let declared = declarations(block);
        let value = |name: &str| {
            declared
                .iter()
                .find(|(declared, _)| declared == name)
                .map(|(_, value)| value.as_str())
        };
        let Some(family) = value("font-family").and_then(first_family) else {
            return;
        };
        self.faces.push(FontFace {
            family,
            bold: value("font-weight").and_then(weight).unwrap_or(false),
            italic: value("font-style").is_some_and(|style| style != "normal"),
            source,
        });
    }

    /// The style of an element: `ancestors` are its open parents, outermost
    /// first, `parent` is the computed style of the nearest one and
    /// `inline` its `style` attribute.
    pub fn compute(
        &self,
        parent: &Computed,
        ancestors: &[Element],
        element: &Element,
        inline: Option<&str>,
    ) -> Computed {
        let mut computed = Computed::inherit(parent);
        if self.rules.is_empty() && inline.is_none() {
            return computed;
        }
        let mut matched: Vec<usize> = Vec::new();
        let mut add = |key: &str| {
            if let Some(rules) = self.index.get(key) {
                matched.extend(rules);
            }
        };
        add("*");
        add(&element.tag);
        if !element.id.is_empty() {
            add(&format!("#{}", element.id));
        }
        for class in &element.classes {
            add(&format!(".{class}"));
        }
        matched.retain(|rule| {
            let mut parts = self.rules[*rule].selector.iter().rev();
            if !parts.next().is_some_and(|last| last.matches(element)) {
                return false;
            }
            let mut outer = ancestors.iter().rev();
            parts.all(|part| outer.any(|ancestor| part.matches(ancestor)))
        });
        matched.sort_by_key(|rule| (self.rules[*rule].specificity, *rule));
        matched.dedup();
        for rule in matched {
            for (name, value) in &self.declarations[self.rules[rule].declarations] {
                apply(&mut computed, parent, name, value);
            }
        }
        if let Some(inline) = inline {
            for (name, value) in declarations(inline) {
                apply(&mut computed, parent, &name, &value);
            }
        }
        computed
    }
}

fn first_family(value: &str) -> Option<String> {
    let family = value
        .split(',')
        .next()?
        .trim()
        .trim_matches(['"', '\''])
        .trim();
    (!family.is_empty()).then(|| family.to_string())
}

fn weight(value: &str) -> Option<bool> {
    match value {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        number => number.parse::<u32>().ok().map(|weight| weight >= 600),
    }
}

/// A length in em; percentages and `auto` have no fixed size.
fn length(value: &str) -> Option<f32> {
    let split = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(value.len());
    let number: f32 = value[..split].parse().ok()?;
    let em = match value[split..].trim() {
        "em" | "rem" => number,
        "ex" | "ch" => number / 2.,
        "px" => number / 16.,
        "pt" => number / 12.,
        "pc" => number,
        "in" => number * 6.,
        "cm" => number * 2.36,
        "mm" => number * 0.236,
        "" if number == 0. => 0.,
        _ => return None,
    };
    em.is_finite().then_some(em.clamp(-20., 20.))
}

fn font_size(value: &str, parent: f32) -> Option<f32> {
    let size = match value {
        "xx-small" => 0.6,
        "x-small" => 0.75,
        "small" => 0.89,
        "medium" => 1.,
        "large" => 1.2,
        "x-large" => 1.5,
        "xx-large" => 2.,
        "smaller" => parent * 0.83,
        "larger" => parent * 1.2,
        other => match other.strip_suffix('%') {
            Some(percent) => parent * percent.trim().parse::<f32>().ok()? / 100.,
            None if other.ends_with("rem") => length(other)?,
            None if other.ends_with("em") => parent * length(other)?,
            None => length(other)?,
        },
    };
    (size.is_finite() && size > 0.).then_some(size.clamp(0.5, 4.))
}

fn apply(computed: &mut Computed, parent: &Computed, name: &str, value: &str) {
    let sides = |value: &str| -> [Option<f32>; 4] {
        let given: Vec<Option<f32>> = value.split_whitespace().map(length).collect();
        match given.as_slice() {
            [all] => [*all; 4],
            [vertical, horizontal] => [*vertical, *horizontal, *vertical, *horizontal],
            [top, horizontal, bottom] => [*top, *horizontal, *bottom, *horizontal],
            [top, right, bottom, left, ..] => [*top, *right, *bottom, *left],
            [] => [None; 4],
        }
    };
    match name {
        "text-align" => {
            computed.align = match value {
                "center" => Some(Align::Center),
                "right" | "end" => Some(Align::End),
                "justify" => Some(Align::Justify),
                "left" | "start" => Some(Align::Start),
                _ => computed.align,
            }
        }
        "font-weight" => computed.bold = weight(value).or(computed.bold),
        "font-style" => {
            computed.italic = match value {
                "italic" | "oblique" => Some(true),
                "normal" => Some(false),
                _ => computed.italic,
            }
        }
        "font-size" => {
            computed.size = font_size(value, parent.size.unwrap_or(1.)).or(computed.size);
        }
        "font-family" => computed.family = first_family(value).or(computed.family.take()),
        "text-indent" => computed.indent = length(value).or(computed.indent),
        "display" => computed.hidden = value == "none",
        "visibility" => computed.hidden |= value == "hidden",
        "text-decoration" | "text-decoration-line" => {
            computed.underline = value.contains("underline");
            computed.strike = value.contains("line-through");
        }
        "vertical-align" => {
            computed.vertical = match value {
                "super" => Some(Vertical::Super),
                "sub" => Some(Vertical::Sub),
                _ => None,
            }
        }
        "margin-top" => computed.margin_top = length(value).unwrap_or(computed.margin_top),
        "margin-bottom" => {
            computed.margin_bottom = length(value).unwrap_or(computed.margin_bottom);
        }
        "margin-left" | "padding-left" => {
            computed.margin_left = length(value).unwrap_or(computed.margin_left);
        }
        "margin" => {
            let [top, _, bottom, left] = sides(value);
            computed.margin_top = top.unwrap_or(computed.margin_top);
            computed.margin_bottom = bottom.unwrap_or(computed.margin_bottom);
            computed.margin_left = left.unwrap_or(computed.margin_left);
        }
        _ => {}
    }
}

/// The family name a TrueType or OpenType font file gives itself, which is
/// the name a text system knows it by once the file is loaded.
pub fn font_family_name(data: &[u8]) -> Option<String> {
    let u16_at = |at: usize| Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?));
    if !matches!(data.get(..4)?, [0, 1, 0, 0] | b"OTTO" | b"true") {
        return None;
    }
    let tables = usize::from(u16_at(4)?);
    let table = (0..tables)
        .map(|index| 12 + index * 16)
        .find(|record| data.get(*record..*record + 4) == Some(b"name"))?;
    let start = u32_at(table + 8)? as usize;
    let count = usize::from(u16_at(start + 2)?);
    let strings = start + usize::from(u16_at(start + 4)?);
    let mut best: Option<(u8, String)> = None;
    for index in 0..count {
        let record = start + 6 + index * 12;
        let (platform, name_id) = (u16_at(record)?, u16_at(record + 6)?);
        // The typographic family when there is one, else the family.
        let rank = match (name_id, platform) {
            (16, 3 | 0) => 4,
            (16, _) => 3,
            (1, 3 | 0) => 2,
            (1, _) => 1,
            _ => continue,
        };
        if best.as_ref().is_some_and(|(have, _)| *have >= rank) {
            continue;
        }
        let from = strings + usize::from(u16_at(record + 10)?);
        let bytes = data.get(from..from + usize::from(u16_at(record + 8)?))?;
        let name = if platform == 1 {
            bytes.iter().map(|byte| char::from(*byte)).collect()
        } else {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        };
        if !name.trim().is_empty() {
            best = Some((rank, name.trim().to_string()));
        }
    }
    best.map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(tag: &str, classes: &[&str]) -> Element {
        Element {
            tag: tag.to_string(),
            id: String::new(),
            classes: classes.iter().map(|class| class.to_string()).collect(),
        }
    }

    #[test]
    fn cascade_follows_specificity_order_and_inheritance() {
        let sheet = Stylesheet::parse(
            "/* c */ @charset \"utf-8\"; p { text-align: justify; text-indent: 1.5em; margin: 0 }\n\
             p.first, h2 > span { text-indent: 0; font-size: 120% }\n\
             .note p { font-style: italic; margin: 1em 0 2em 3em }\n\
             p { text-align: left }\n\
             a:hover { font-weight: bold }\n\
             @media print { p { display: none } }\n\
             @media screen and (min-width: 1px) { .gone { display: none } }\n\
             @font-face { font-family: \"My Serif\"; font-weight: 700; src: url(Fonts/A.ttf) }",
        );
        let root = Computed::default();
        let plain = sheet.compute(&root, &[], &element("p", &[]), None);
        assert_eq!(plain.align, Some(Align::Start));
        assert_eq!(plain.indent, Some(1.5));
        assert!(!plain.hidden);

        let first = sheet.compute(&root, &[], &element("p", &["first"]), None);
        assert_eq!((first.indent, first.size), (Some(0.), Some(1.2)));

        let note = [element("div", &["note"]), element("blockquote", &[])];
        let inside = sheet.compute(&root, &note, &element("p", &[]), Some("font-size: 0.5em"));
        assert_eq!(inside.italic, Some(true));
        assert_eq!(
            (inside.margin_top, inside.margin_bottom, inside.margin_left),
            (1., 2., 3.)
        );
        assert_eq!(inside.size, Some(0.5));
        // Inherited: size and slant, not margins.
        let child = sheet.compute(&inside, &note, &element("span", &[]), None);
        assert_eq!(
            (child.italic, child.size, child.margin_left),
            (Some(true), Some(0.5), 0.)
        );

        assert!(
            sheet
                .compute(&root, &[], &element("p", &["gone"]), None)
                .hidden
        );
        assert_eq!(
            sheet.compute(&root, &[], &element("a", &[]), None).bold,
            None
        );
        assert_eq!(
            sheet.faces,
            vec![FontFace {
                family: "my serif".into(),
                bold: true,
                italic: false,
                source: "Fonts/A.ttf".into()
            }]
        );
    }

    #[test]
    fn urls_are_rewritten_and_imports_dropped() {
        let css = "@import url(other.css);\n@font-face { src: url( '../Fonts/a.ttf' ) }\np { background: URL(x.png) }";
        let rewritten = rewrite_urls(css, &mut |target| {
            target.ends_with(".ttf").then(|| "fonts/a.ttf".to_string())
        });
        assert_eq!(
            rewritten,
            "\n@font-face { src: url(\"fonts/a.ttf\") }\np { background: url(\"\") }"
        );
    }
}
