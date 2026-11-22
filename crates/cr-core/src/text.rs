//! Text helpers that reproduce calibre's sorting and file-name conventions.
//!
//! The functions here are deliberately dependency free and heavily tested:
//! `title_sort`, `author_sort` and `ascii_filename` decide where books live
//! inside a calibre library, so getting them right keeps libraries
//! interchangeable with calibre itself.

use std::path::Path;

/// Articles moved to the end of a title when sorting (calibre's English default).
const TITLE_ARTICLES: [&str; 3] = ["The", "A", "An"];

/// Honorifics ignored while locating the surname.
const NAME_PREFIXES: [&str; 7] = ["mr", "mrs", "ms", "dr", "prof", "sir", "dame"];

/// Generational/professional suffixes kept at the end of a sort name.
const NAME_SUFFIXES: [&str; 11] = [
    "jr", "sr", "inc", "ph.d", "phd", "md", "m.d", "i", "ii", "iii", "iv",
];

/// Words that signal an organisation rather than a person; calibre leaves
/// these names untouched.
const COPY_WORDS: [&str; 17] = [
    "agency",
    "corporation",
    "company",
    "co.",
    "council",
    "committee",
    "inc.",
    "institute",
    "national",
    "society",
    "club",
    "team",
    "software",
    "games",
    "entertainment",
    "media",
    "studios",
];

/// calibre's default title sorting: `The Hobbit` -> `Hobbit, The`.
pub fn title_sort(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Strip a matched pair of surrounding quotes, as calibre does.
    let unquoted = strip_matching_quotes(trimmed);
    for article in TITLE_ARTICLES {
        if let Some(rest) = strip_prefix_ignore_case(unquoted, article) {
            if rest.starts_with(char::is_whitespace) {
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    return format!("{rest}, {article}");
                }
            }
        }
    }
    unquoted.to_string()
}

/// calibre's default author sorting (`comma` method): `John Smith` ->
/// `Smith, John`, while `Smith, John` is already sorted and left alone.
pub fn author_sort(author: &str) -> String {
    let author = author.trim();
    if author.is_empty() {
        return String::new();
    }
    let without_brackets = remove_bracketed_text(author);
    let core = without_brackets.trim();

    // Already in "Last, First" form.
    if core.contains(',') {
        return author.to_string();
    }

    let tokens: Vec<&str> = core.split_whitespace().collect();
    if tokens.len() < 2 {
        return author.to_string();
    }

    let lowered: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();

    // Organisations such as "Acme Inc." sort by their full name.
    if lowered.iter().any(|t| COPY_WORDS.contains(&t.as_str())) {
        return author.to_string();
    }

    // Skip leading honorifics.
    let mut first = 0;
    while first < tokens.len() && is_prefix(&lowered[first]) {
        first += 1;
    }
    if first >= tokens.len() {
        return author.to_string();
    }

    // Skip trailing generational suffixes.
    let mut last = tokens.len() - 1;
    while last > first && is_suffix(&lowered[last]) {
        last -= 1;
    }

    let suffix = tokens[last + 1..].join(" ");
    let surname = tokens[last];
    let given = tokens[first..last].join(" ");

    let mut out = String::new();
    out.push_str(surname);
    if !given.is_empty() {
        out.push_str(", ");
        out.push_str(&given);
    }
    if !suffix.is_empty() {
        out.push(' ');
        out.push_str(&suffix);
    }
    out
}

/// The `author_sort` value calibre stores on a book: the per-author sort
/// values joined by ` & `.
pub fn book_author_sort(authors: &[String]) -> String {
    authors
        .iter()
        .map(|a| author_sort(a))
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(" & ")
}

/// Convert a string to a safe ASCII path component, transliterating common
/// Latin characters the way calibre's `ascii_filename` does.
pub fn ascii_filename(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if let Some(replacement) = transliterate(ch) {
            out.push_str(replacement);
        } else if ch.is_ascii() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    sanitize_component(&out)
}

/// Strip characters that are illegal in a path component on any platform.
pub fn sanitize_component(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect();
    // Windows strips trailing dots and spaces.
    while out.ends_with(' ') || out.ends_with('.') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("Unknown");
    }
    if is_windows_reserved(&out) {
        out.push('w');
    }
    out
}

/// Book directory relative to the library root, as calibre names it:
/// `First Author/Title (id)`.
pub fn book_dir_name(id: i64, title: &str, first_author: &str) -> String {
    let author = truncate_component(&ascii_filename(first_author.trim()), 100);
    let author = if author.is_empty() {
        "Unknown".to_string()
    } else {
        author
    };
    let title = truncate_component(&ascii_filename(title.trim()), 100);
    let title = if title.is_empty() {
        "Unknown".to_string()
    } else {
        title
    };
    format!("{author}/{title} ({id})")
}

/// Canonical format file stem used inside a calibre book directory:
/// `Title - Author`.
pub fn format_file_stem(title: &str, author: &str) -> String {
    let title = truncate_component(&ascii_filename(title.trim()), 100);
    let author = truncate_component(&ascii_filename(author.trim()), 60);
    let mut stem = if author.is_empty() {
        title
    } else {
        format!("{title} - {author}")
    };
    while stem.ends_with('.') || stem.ends_with(' ') {
        stem.pop();
    }
    if stem.is_empty() {
        "Unknown".to_string()
    } else {
        stem
    }
}

/// Split a user supplied author list using calibre's rules: ampersands,
/// semicolons and the words "and"/"with" all separate names.
pub fn split_authors(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in input.split(['&', ';']) {
        for part in split_on_and(chunk) {
            let name = part.trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Format a calibre series index: integers without decimals, otherwise up to
/// two decimal places (`1`, `1.5`, `2.25`).
pub fn format_series_index(index: f64) -> String {
    if index.fract().abs() < f64::EPSILON {
        format!("{}", index as i64)
    } else {
        let mut s = format!("{index:.2}");
        while s.ends_with('0') {
            s.pop();
        }
        s
    }
}

/// Parse a series index from user input; empty means `1.0` like calibre.
pub fn parse_series_index(input: &str) -> Option<f64> {
    let input = input.trim();
    if input.is_empty() {
        return Some(1.0);
    }
    input.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Return the file extension in calibre's canonical upper-case form.
pub fn format_from_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.trim().to_ascii_uppercase();
    if ext.is_empty() {
        None
    } else {
        Some(ext)
    }
}

/// Remove parenthesised/bracketed text (`John Smith (editor)` -> `John Smith`).
fn remove_bracketed_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut depth = 0usize;
    for ch in input.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn is_prefix(lowered: &str) -> bool {
    let trimmed = lowered.trim_end_matches('.');
    NAME_PREFIXES.contains(&trimmed)
}

fn is_suffix(lowered: &str) -> bool {
    let trimmed = lowered.trim_end_matches('.');
    NAME_SUFFIXES.contains(&trimmed)
}

fn strip_prefix_ignore_case<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    let input_lower = input.to_lowercase();
    if input_lower.starts_with(&prefix.to_lowercase()) {
        // Safe: prefix is ASCII, so its byte length matches in both strings
        // only if the input prefix is ASCII. Guard with get().
        input.get(prefix.len()..)
    } else {
        None
    }
}

fn strip_matching_quotes(input: &str) -> &str {
    let pairs = [
        ('"', '"'),
        ('\'', '\''),
        ('\u{201c}', '\u{201d}'),
        ('\u{2018}', '\u{2019}'),
    ];
    for (open, close) in pairs {
        if input.starts_with(open) && input.ends_with(close) && input.chars().count() > 2 {
            return &input[open.len_utf8()..input.len() - close.len_utf8()];
        }
    }
    input
}

fn split_on_and(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = input;
    loop {
        let lower = rest.to_lowercase();
        let found = [" and ", " with ", " AND ", " WITH "]
            .iter()
            .filter_map(|needle| {
                lower
                    .find(&needle.to_lowercase())
                    .map(|idx| (idx, needle.len()))
            })
            .min_by_key(|(idx, _)| *idx);
        match found {
            Some((idx, len)) => {
                out.push(rest[..idx].to_string());
                rest = &rest[idx + len..];
            }
            None => {
                out.push(rest.to_string());
                break;
            }
        }
    }
    out
}

fn truncate_component(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_string();
    }
    input
        .chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn is_windows_reserved(name: &str) -> bool {
    let upper = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn transliterate(ch: char) -> Option<&'static str> {
    Some(match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'æ' => "ae",
        'Æ' => "AE",
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'Ç' | 'Ć' | 'Č' | 'Ĉ' | 'Ċ' => "C",
        'ď' | 'đ' | 'ð' => "d",
        'Ď' | 'Đ' | 'Ð' => "D",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => "E",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => "G",
        'ĥ' | 'ħ' => "h",
        'Ĥ' | 'Ħ' => "H",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => "I",
        'ĵ' => "j",
        'Ĵ' => "J",
        'ķ' => "k",
        'Ķ' => "K",
        'ĺ' | 'ļ' | 'ľ' | 'ł' => "l",
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ł' => "L",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' => "N",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ơ' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' | 'Ơ' => "O",
        'œ' => "oe",
        'Œ' => "OE",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'Ŕ' | 'Ŗ' | 'Ř' => "R",
        'ś' | 'ş' | 'š' | 'ș' => "s",
        'Ś' | 'Ş' | 'Š' | 'Ș' => "S",
        'ß' => "ss",
        'ţ' | 'ť' | 'ț' | 'þ' => "t",
        'Ţ' | 'Ť' | 'Ț' | 'Þ' => "T",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ư' => "u",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' | 'Ư' => "U",
        'ŵ' => "w",
        'Ŵ' => "W",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'Ý' | 'Ÿ' | 'Ŷ' => "Y",
        'ź' | 'ż' | 'ž' => "z",
        'Ź' | 'Ż' | 'Ž' => "Z",
        '\u{2018}' | '\u{2019}' | '\u{2032}' => "'",
        '\u{201c}' | '\u{201d}' | '\u{2033}' => "\"",
        '\u{2013}' | '\u{2014}' => "-",
        '\u{2026}' => "...",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_titles_like_calibre() {
        assert_eq!(title_sort("The Hobbit"), "Hobbit, The");
        assert_eq!(title_sort("A Game of Thrones"), "Game of Thrones, A");
        assert_eq!(title_sort("Anansi Boys"), "Anansi Boys");
        assert_eq!(
            title_sort("  The   Left Hand of Darkness "),
            "Left Hand of Darkness, The"
        );
        assert_eq!(title_sort("\"Dune\""), "Dune");
        assert_eq!(title_sort(""), "");
    }

    #[test]
    fn sorts_authors_like_calibre() {
        assert_eq!(author_sort("John Smith"), "Smith, John");
        assert_eq!(author_sort("Smith, John"), "Smith, John");
        assert_eq!(author_sort("Ursula K. Le Guin"), "Guin, Ursula K. Le");
        assert_eq!(
            author_sort("Dr. Martin Luther King Jr."),
            "King, Martin Luther Jr."
        );
        assert_eq!(author_sort("Acme Inc."), "Acme Inc.");
        assert_eq!(author_sort("Plato"), "Plato");
        assert_eq!(author_sort("Anonymous"), "Anonymous");
        assert_eq!(author_sort("John Smith (editor)"), "Smith, John");
    }

    #[test]
    fn joins_book_author_sorts() {
        let authors = vec!["Neil Gaiman".to_string(), "Terry Pratchett".to_string()];
        assert_eq!(
            book_author_sort(&authors),
            "Gaiman, Neil & Pratchett, Terry"
        );
    }

    #[test]
    fn makes_ascii_filenames() {
        assert_eq!(ascii_filename("Émile Zola"), "Emile Zola");
        assert_eq!(ascii_filename("A/B: C?"), "A_B_ C_");
        assert_eq!(ascii_filename("trailing... "), "trailing");
        assert_eq!(ascii_filename("CON"), "CONw");
        assert_eq!(ascii_filename("日本"), "__");
    }

    #[test]
    fn builds_calibre_paths() {
        assert_eq!(
            book_dir_name(42, "The Hobbit", "J. R. R. Tolkien"),
            "J. R. R. Tolkien/The Hobbit (42)"
        );
        assert_eq!(
            format_file_stem("The Hobbit", "J. R. R. Tolkien"),
            "The Hobbit - J. R. R. Tolkien"
        );
    }

    #[test]
    fn splits_authors() {
        assert_eq!(
            split_authors("Neil Gaiman & Terry Pratchett"),
            vec!["Neil Gaiman", "Terry Pratchett"]
        );
        assert_eq!(
            split_authors("A. Author and B. Author; C. Author"),
            vec!["A. Author", "B. Author", "C. Author"]
        );
    }

    #[test]
    fn formats_series_indices() {
        assert_eq!(format_series_index(1.0), "1");
        assert_eq!(format_series_index(2.5), "2.5");
        assert_eq!(format_series_index(2.25), "2.25");
        assert_eq!(parse_series_index("3.5"), Some(3.5));
        assert_eq!(parse_series_index(""), Some(1.0));
        assert_eq!(parse_series_index("nope"), None);
    }
}
