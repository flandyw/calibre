//! The search language, compiled to a SQL `WHERE` clause over `books b`.
//!
//! Follows calibre's syntax: terms are ANDed unless joined with `or`; `not`
//! (or a leading `!`/`-`) negates; parentheses group. A term is free text or
//! `field:value`, where the value may start with `=` (exact match) or `~`
//! (regular expression), be `true`/`false` (field set or empty), or for
//! numbers and dates carry a comparison such as `>=2020-06`.

use std::collections::BTreeMap;

use chrono::{Datelike, Duration, NaiveDate, Utc};

use super::BookSort;

/// A custom column a `#label:` term can refer to.
#[derive(Debug, Clone)]
pub(crate) struct CustomField {
    pub id: i64,
    pub label: String,
    pub datatype: String,
}

impl CustomField {
    /// Columns whose values live in their own table, linked to books.
    pub(crate) fn is_normalized(&self) -> bool {
        matches!(
            self.datatype.as_str(),
            "text" | "series" | "enumeration" | "rating"
        )
    }
}

/// What a query may refer to besides the built-in fields.
#[derive(Debug, Default)]
pub(crate) struct SearchContext {
    pub custom: Vec<CustomField>,
    /// Saved searches by lower-cased name, for `search:name`.
    pub saved: BTreeMap<String, String>,
    /// Virtual libraries by lower-cased name, for `vl:name`.
    pub virtual_libraries: BTreeMap<String, String>,
}

pub(crate) struct ParsedQuery {
    pub where_sql: String,
    pub params: Vec<String>,
    pub sort: Option<BookSort>,
    pub descending: bool,
}

pub(crate) fn compile(query: &str, context: &SearchContext) -> ParsedQuery {
    let mut parser = Parser {
        tokens: tokenize(query),
        position: 0,
        context,
        params: Vec::new(),
        sort: None,
        descending: false,
        depth: 0,
    };
    let where_sql = parser.expression();
    ParsedQuery {
        where_sql: if where_sql == TRUE {
            String::new()
        } else {
            where_sql
        },
        params: parser.params,
        sort: parser.sort,
        descending: parser.descending,
    }
}

const TRUE: &str = "1";
const FALSE: &str = "0";
/// How deep saved searches may refer to each other.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Open,
    Close,
    /// A term; `bare` when no part of it was quoted, so `and`/`or`/`not`
    /// count as operators.
    Word {
        text: String,
        bare: bool,
    },
}

fn tokenize(query: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut bare = true;
    let flush = |current: &mut String, bare: &mut bool, out: &mut Vec<Token>| {
        if !current.is_empty() {
            let text = std::mem::take(current);
            out.push(Token::Word { text, bare: *bare });
        }
        *bare = true;
    };
    let mut chars = query.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if quoted && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => {
                quoted = !quoted;
                bare = false;
            }
            c if quoted => current.push(c),
            c if c.is_whitespace() => flush(&mut current, &mut bare, &mut out),
            '(' => {
                flush(&mut current, &mut bare, &mut out);
                out.push(Token::Open);
            }
            ')' => {
                flush(&mut current, &mut bare, &mut out);
                out.push(Token::Close);
            }
            c => current.push(c),
        }
    }
    flush(&mut current, &mut bare, &mut out);
    out
}

struct Parser<'a> {
    tokens: Vec<Token>,
    position: usize,
    context: &'a SearchContext,
    params: Vec<String>,
    sort: Option<BookSort>,
    descending: bool,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Word { text, bare: true }) if text.eq_ignore_ascii_case(word))
    }

    fn expression(&mut self) -> String {
        let mut alternatives = vec![self.conjunction()];
        while self.keyword("or") {
            self.position += 1;
            alternatives.push(self.conjunction());
        }
        if alternatives.len() == 1 {
            return alternatives.remove(0);
        }
        format!("({})", alternatives.join(" OR "))
    }

    fn conjunction(&mut self) -> String {
        let mut parts = Vec::new();
        loop {
            if self.keyword("and") {
                self.position += 1;
                continue;
            }
            if self.keyword("or") || matches!(self.peek(), None | Some(Token::Close)) {
                break;
            }
            let part = self.negation();
            if part != TRUE {
                parts.push(part);
            }
        }
        match parts.len() {
            0 => TRUE.to_string(),
            1 => parts.remove(0),
            _ => format!("({})", parts.join(" AND ")),
        }
    }

    fn negation(&mut self) -> String {
        if self.keyword("not") && self.position + 1 < self.tokens.len() {
            self.position += 1;
            return format!("NOT {}", self.negation());
        }
        match self.tokens.get(self.position).cloned() {
            Some(Token::Open) => {
                self.position += 1;
                let inner = self.expression();
                if self.peek() == Some(&Token::Close) {
                    self.position += 1;
                }
                inner
            }
            Some(Token::Word { text, .. }) => {
                self.position += 1;
                self.term(&text)
            }
            Some(Token::Close) | None => TRUE.to_string(),
        }
    }

    fn term(&mut self, raw: &str) -> String {
        let (negated, token) = strip_negation(raw);
        let clause = match split_field(token) {
            Some(("sort", value)) => {
                let (descending, name) = match value.strip_prefix('-') {
                    Some(name) => (true, name),
                    None => (false, value),
                };
                if let Some(sort) = BookSort::parse(name) {
                    self.sort = Some(sort);
                    self.descending = descending;
                }
                return TRUE.to_string();
            }
            Some((field, value)) => match self.field(&field.to_ascii_lowercase(), value) {
                Some(clause) => clause,
                None => self.free_text(token),
            },
            None => self.free_text(token),
        };
        if negated {
            format!("NOT ({clause})")
        } else {
            clause
        }
    }

    /// Compile another query in place (a saved search or virtual library).
    fn nested(&mut self, query: Option<&String>) -> String {
        let Some(query) = query else {
            return FALSE.to_string();
        };
        if self.depth >= MAX_DEPTH {
            return FALSE.to_string();
        }
        let outer = std::mem::replace(&mut self.tokens, tokenize(query));
        let position = std::mem::replace(&mut self.position, 0);
        self.depth += 1;
        let clause = self.expression();
        self.depth -= 1;
        self.tokens = outer;
        self.position = position;
        format!("({clause})")
    }

    fn free_text(&mut self, value: &str) -> String {
        let parts: Vec<String> = ["title", "authors", "tags", "series", "publisher"]
            .iter()
            .filter_map(|field| self.field(field, value))
            .collect();
        format!("({})", parts.join(" OR "))
    }

    fn field(&mut self, field: &str, value: &str) -> Option<String> {
        let context = self.context;
        let clause = match field {
            "search" => self.nested(context.saved.get(&value.to_lowercase())),
            "vl" => self.nested(context.virtual_libraries.get(&value.to_lowercase())),
            "title" => self.text(None, "b.title", value),
            "author" | "authors" => self.text(
                Some("books_authors_link l JOIN authors x ON x.id=l.author WHERE l.book=b.id"),
                "REPLACE(x.name, '|', ',')",
                value,
            ),
            "tag" | "tags" => self.text(
                Some("books_tags_link l JOIN tags x ON x.id=l.tag WHERE l.book=b.id"),
                "x.name",
                value,
            ),
            "series" => self.text(
                Some("books_series_link l JOIN series x ON x.id=l.series WHERE l.book=b.id"),
                "x.name",
                value,
            ),
            "publisher" => self.text(
                Some(
                    "books_publishers_link l JOIN publishers x ON x.id=l.publisher \
                     WHERE l.book=b.id",
                ),
                "x.name",
                value,
            ),
            "language" | "languages" | "lang" => self.text(
                Some(
                    "books_languages_link l JOIN languages x ON x.id=l.lang_code \
                     WHERE l.book=b.id",
                ),
                "x.lang_code",
                value,
            ),
            "comment" | "comments" => {
                self.text(Some("comments x WHERE x.book=b.id"), "x.text", value)
            }
            "format" | "formats" => {
                // A bare format name means exactly that format.
                let exact = format!("={value}");
                let value = match value {
                    "true" | "false" => value,
                    v if v.starts_with(['=', '~']) => v,
                    _ => exact.as_str(),
                };
                self.text(Some("data x WHERE x.book=b.id"), "x.format", value)
            }
            "identifier" | "identifiers" => match value.split_once(':') {
                Some((kind, wanted)) => {
                    self.params.push(kind.to_ascii_lowercase());
                    let source = "identifiers x WHERE x.book=b.id AND LOWER(x.type)=?";
                    self.text(Some(source), "x.val", wanted)
                }
                None => self.text(Some("identifiers x WHERE x.book=b.id"), "x.val", value),
            },
            "cover" => match value {
                "true" => "b.has_cover=1".to_string(),
                "false" => "b.has_cover=0".to_string(),
                _ => return None,
            },
            "rating" => {
                let column = "COALESCE((SELECT r.rating FROM ratings r JOIN books_ratings_link l \
                              ON l.rating=r.id WHERE l.book=b.id), 0)";
                self.number(column, value, 2.0)
            }
            "series_index" => self.number("b.series_index", value, 1.0),
            "size" => self.number(
                "COALESCE((SELECT MAX(d.uncompressed_size) FROM data d WHERE d.book=b.id), 0)",
                value,
                1.0,
            ),
            "id" => self.number("b.id", value, 1.0),
            "date" | "timestamp" => self.date("b.timestamp", value),
            "pubdate" => self.date("b.pubdate", value),
            "last_modified" => self.date("b.last_modified", value),
            custom if custom.starts_with('#') => {
                let label = &custom[1..];
                let column = context
                    .custom
                    .iter()
                    .find(|column| column.label.eq_ignore_ascii_case(label));
                match column {
                    Some(column) => self.custom(column, value),
                    None => FALSE.to_string(),
                }
            }
            _ => return None,
        };
        Some(clause)
    }

    fn custom(&mut self, column: &CustomField, value: &str) -> String {
        let id = column.id;
        let plain = format!("custom_column_{id} x WHERE x.book=b.id");
        let single = format!("(SELECT x.value FROM {plain})");
        match column.datatype.as_str() {
            "int" | "float" => match value {
                "true" => format!("{single} IS NOT NULL"),
                "false" => format!("{single} IS NULL"),
                _ => self.number(&single, value, 1.0),
            },
            "bool" => match value.to_ascii_lowercase().as_str() {
                "true" | "yes" | "checked" => format!("{single}=1"),
                "false" | "no" | "unchecked" => format!("{single}=0"),
                "empty" | "blank" => format!("{single} IS NULL"),
                _ => FALSE.to_string(),
            },
            "datetime" => match value {
                "true" => format!("{single} IS NOT NULL"),
                "false" => format!("{single} IS NULL"),
                _ => self.date(&single, value),
            },
            "rating" => {
                let column = format!(
                    "COALESCE((SELECT x.value FROM books_custom_column_{id}_link l \
                     JOIN custom_column_{id} x ON x.id=l.value WHERE l.book=b.id), 0)"
                );
                self.number(&column, value, 2.0)
            }
            _ if column.is_normalized() => {
                let source = format!(
                    "books_custom_column_{id}_link l JOIN custom_column_{id} x \
                     ON x.id=l.value WHERE l.book=b.id"
                );
                self.text(Some(&source), "x.value", value)
            }
            _ => self.text(Some(&plain), "x.value", value),
        }
    }

    /// Match text in `column`, which comes from `source` (a `FROM ... WHERE`
    /// fragment tied to the book) or, without one, from the book row itself.
    fn text(&mut self, source: Option<&str>, column: &str, value: &str) -> String {
        let exists = |test: &str| match source {
            Some(source) => format!("EXISTS (SELECT 1 FROM {source} AND {test})"),
            None => test.to_string(),
        };
        match value {
            "true" => return exists(&format!("COALESCE({column}, '') <> ''")),
            "false" => return format!("NOT {}", exists(&format!("COALESCE({column}, '') <> ''"))),
            _ => {}
        }
        let test = if let Some(exact) = value.strip_prefix('=') {
            self.params.push(exact.to_string());
            format!("{column} = ? COLLATE NOCASE")
        } else if let Some(pattern) = value.strip_prefix('~') {
            self.params.push(pattern.to_string());
            format!("{column} REGEXP ?")
        } else {
            self.params.push(like_pattern(value));
            format!("{column} LIKE ? ESCAPE '\\'")
        };
        exists(&test)
    }

    /// Compare a number; `scale` converts what the user types to what is
    /// stored (ratings are stored doubled).
    fn number(&mut self, column: &str, value: &str, scale: f64) -> String {
        match value {
            "true" => return format!("{column} > 0"),
            "false" => return format!("{column} = 0"),
            _ => {}
        }
        let (operator, rest) = split_operator(value);
        let Some(number) = parse_number(rest) else {
            return FALSE.to_string();
        };
        self.params.push((number * scale).to_string());
        format!("{column} {operator} CAST(? AS REAL)")
    }

    fn date(&mut self, column: &str, value: &str) -> String {
        let (operator, rest) = split_operator(value);
        let Some((start, end)) = date_range(rest, Utc::now().date_naive()) else {
            return FALSE.to_string();
        };
        let day = format!("substr({column}, 1, 10)");
        let (start, end) = (start.to_string(), end.to_string());
        let mut compare = |test: &str, bound: String| {
            self.params.push(bound);
            format!("{day} {test} ?")
        };
        match operator {
            ">" => compare(">=", end),
            ">=" => compare(">=", start),
            "<" => compare("<", start),
            "<=" => compare("<", end),
            "<>" => format!("NOT ({} AND {})", compare(">=", start), compare("<", end)),
            _ => format!("({} AND {})", compare(">=", start), compare("<", end)),
        }
    }
}

fn split_field(token: &str) -> Option<(&str, &str)> {
    let (field, value) = token.split_once(':')?;
    let named = !field.is_empty()
        && field
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '#');
    (named && !value.is_empty()).then_some((field, value))
}

fn strip_negation(token: &str) -> (bool, &str) {
    if let Some(rest) = token.strip_prefix('!') {
        if !rest.is_empty() {
            return (true, rest);
        }
    }
    if let Some(rest) = token.strip_prefix('-') {
        if !rest.is_empty() && !rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return (true, rest);
        }
    }
    (false, token)
}

/// Split a leading comparison off a value, as its SQL operator.
fn split_operator(value: &str) -> (&'static str, &str) {
    for (prefix, operator) in [
        (">=", ">="),
        ("<=", "<="),
        ("!=", "<>"),
        (">", ">"),
        ("<", "<"),
        ("=", "="),
    ] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return (operator, rest.trim());
        }
    }
    ("=", value.trim())
}

/// A number with an optional `k`, `M` or `G` size suffix.
fn parse_number(value: &str) -> Option<f64> {
    let (digits, factor) = match value.chars().last()? {
        'k' | 'K' => (&value[..value.len() - 1], 1024.0),
        'm' | 'M' => (&value[..value.len() - 1], 1024.0 * 1024.0),
        'g' | 'G' => (&value[..value.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (value, 1.0),
    };
    let number: f64 = digits.trim().parse().ok()?;
    number.is_finite().then_some(number * factor)
}

/// The days a date expression covers, as `[start, end)`.
fn date_range(value: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let one_day = |day: NaiveDate| Some((day, day + Duration::days(1)));
    let month = |year: i32, month: u32| {
        let start = NaiveDate::from_ymd_opt(year, month, 1)?;
        let (next_year, next_month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        Some((start, NaiveDate::from_ymd_opt(next_year, next_month, 1)?))
    };
    let lowered = value.to_ascii_lowercase();
    match lowered.as_str() {
        "today" => return one_day(today),
        "yesterday" => return one_day(today - Duration::days(1)),
        "thismonth" => return month(today.year(), today.month()),
        _ => {}
    }
    if let Some(days) = lowered.strip_suffix("daysago") {
        let days: i64 = days.trim().parse().ok()?;
        return one_day(today.checked_sub_signed(Duration::try_days(days)?)?);
    }
    let mut parts = lowered.split(['-', '/']);
    let year: i32 = parts.next()?.parse().ok()?;
    let month_number: Option<u32> = parts.next().map(|part| part.parse()).transpose().ok()?;
    let day: Option<u32> = parts.next().map(|part| part.parse()).transpose().ok()?;
    match (month_number, day) {
        (None, _) => Some((
            NaiveDate::from_ymd_opt(year, 1, 1)?,
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?,
        )),
        (Some(number), None) => month(year, number),
        (Some(number), Some(day)) => one_day(NaiveDate::from_ymd_opt(year, number, day)?),
    }
}

pub(crate) fn like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('%');
    for ch in value.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped.push('%');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(query: &str) -> ParsedQuery {
        compile(query, &SearchContext::default())
    }

    #[test]
    fn parses_fields_free_text_and_sort() {
        let parsed = parse("author:tolkien tag:fantasy hobbit sort:-series");
        assert_eq!(parsed.params.len(), 7);
        assert_eq!(parsed.params[0], "%tolkien%");
        assert_eq!(parsed.sort, Some(BookSort::Series));
        assert!(parsed.descending);
    }

    #[test]
    fn supports_negation_and_quoted_values() {
        let parsed = parse("!tag:romance title:\"the hobbit\"");
        assert!(parsed.where_sql.starts_with("(NOT ("));
        assert_eq!(parsed.params, ["%romance%", "%the hobbit%"]);
        // Quoted operators are ordinary words.
        assert_eq!(parse("\"or\"").params.len(), 5);
    }

    #[test]
    fn groups_alternatives() {
        let parsed = parse("tag:a or (tag:b and not tag:c)");
        assert_eq!(parsed.params, ["%a%", "%b%", "%c%"]);
        assert!(parsed.where_sql.contains(" OR "));
        assert!(parsed.where_sql.contains("AND NOT EXISTS"));
        assert!(parse("").where_sql.is_empty());
        assert!(parse("( )").where_sql.is_empty());
    }

    #[test]
    fn reads_match_kinds_and_comparisons() {
        assert_eq!(parse("tag:=sf").params, ["sf"]);
        assert!(parse("title:~^the").where_sql.contains("REGEXP"));
        assert_eq!(parse("rating:>=3").params, ["6"]);
        assert_eq!(parse("size:>1k").params, ["1024"]);
        assert_eq!(
            parse("pubdate:2020-12").params,
            ["2020-12-01", "2021-01-01"]
        );
        assert_eq!(parse("date:>2020").params, ["2021-01-01"]);
        assert_eq!(parse("rating:abc").where_sql, "0");
    }

    #[test]
    fn expands_saved_searches_without_looping() {
        let mut context = SearchContext::default();
        context
            .saved
            .insert("sf".into(), "tag:scifi or tag:sf".into());
        context.saved.insert("loop".into(), "search:loop".into());
        assert_eq!(compile("search:SF", &context).params, ["%scifi%", "%sf%"]);
        assert!(compile("search:loop", &context).params.is_empty());
        assert_eq!(compile("search:missing", &context).where_sql, "0");
    }

    #[test]
    fn builds_safe_like_patterns() {
        assert_eq!(like_pattern("100%_safe"), "%100\\%\\_safe%");
    }
}
