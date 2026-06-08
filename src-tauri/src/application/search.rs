//! Search options and the compiled query matcher.
//!
//! The query language mirrors voidtools Everything: space-separated terms are
//! AND-combined, `|` is OR, `!` negates the following term, and `"quoted"`
//! text is a literal phrase (escaping the operators). `*`/`?` wildcards,
//! whole-word and regex modes, case sensitivity, full-path matching, and the
//! `ext:`, `path:`, `file:`, `folder:`, `size:`, `dm:`/`dc:`/`da:` (modified /
//! created / accessed dates) and `attrib:` functions are all supported as leaf
//! predicates, so e.g. `ext:dll | ext:exe`, `report !draft`, `"my file"
//! size:>1mb`, `dm:today`, `dc:2024-01-01..2024-06-30` and `attrib:h` all work.

use chrono::{Datelike, Days, Local, Months, NaiveDate, TimeZone};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

/// Which column results are ordered by.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Path,
    Size,
    Modified,
    Created,
    Accessed,
    Ext,
    Attributes,
}

/// The entry fields a [`Matcher`] inspects. `path` / `path_lower` may be empty
/// strings when [`Matcher::needs_path`] is false. Borrowed, so building one per
/// candidate entry allocates nothing.
pub struct EntryView<'a> {
    pub name: &'a str,
    pub name_lower: &'a str,
    pub path: &'a str,
    pub path_lower: &'a str,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified_ms: Option<i64>,
    pub created_ms: Option<i64>,
    pub accessed_ms: Option<i64>,
    pub attributes: u32,
}

/// Everything-style search options sent from the UI (or a service client).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchOptions {
    pub query: String,
    pub limit: usize,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub match_path: bool,
    pub sort: SortKey,
    pub ascending: bool,
    /// Group directories ahead of files, regardless of the sort column/direction.
    pub folders_first: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            limit: 5000,
            match_case: false,
            whole_word: false,
            regex: false,
            match_path: false,
            sort: SortKey::Name,
            ascending: true,
            folders_first: false,
        }
    }
}

/// Inclusive byte-size bounds from a `size:` operator.
#[derive(Default)]
struct SizeFilter {
    min: Option<u64>,
    max: Option<u64>,
}

/// Which timestamp a date predicate tests.
#[derive(Clone, Copy)]
enum DateField {
    Modified,
    Created,
    Accessed,
}

/// Inclusive Unix-millisecond bounds from a `dm:`/`dc:`/`da:` operator.
struct DateFilter {
    min: Option<i64>,
    max: Option<i64>,
}

/// Whether a text predicate matches the file name or the full path.
#[derive(Clone, Copy)]
enum TextTarget {
    Name,
    Path,
}

enum TextKind {
    /// Substring; pre-cased to match the haystack casing.
    Plain(String),
    /// Compiled wildcard / whole-word / regex pattern (case flag baked in).
    Re(Regex),
}

/// A single test against one entry.
enum Pred {
    Text {
        target: TextTarget,
        kind: TextKind,
    },
    Ext(Vec<String>),
    Size(SizeFilter),
    Date {
        field: DateField,
        filter: DateFilter,
    },
    /// All bits in the mask must be set in the entry's attributes.
    Attrib(u32),
    File,
    Folder,
}

/// A predicate plus optional negation (`!`).
struct Leaf {
    negate: bool,
    pred: Pred,
}

/// A conjunction — all leaves must match (AND).
struct Conjunction {
    leaves: Vec<Leaf>,
}

/// A compiled query: a disjunction of conjunctions (OR of ANDs). An empty set
/// of clauses matches everything.
pub struct Matcher {
    clauses: Vec<Conjunction>,
    match_case: bool,
    needs_path: bool,
}

enum Token {
    Word(String),
    Phrase(String),
    Or,
    Not,
}

impl Matcher {
    /// Compile search options into a matcher, or return a user-facing error
    /// (e.g. an invalid regex).
    pub fn compile(opts: &SearchOptions) -> Result<Self, String> {
        let case = opts.match_case;

        // Regex mode treats the whole query as one expression.
        if opts.regex {
            let query = opts.query.trim();
            if query.is_empty() {
                return Ok(Self {
                    clauses: Vec::new(),
                    match_case: case,
                    needs_path: false,
                });
            }
            let target = if opts.match_path {
                TextTarget::Path
            } else {
                TextTarget::Name
            };
            let leaf = Leaf {
                negate: false,
                pred: Pred::Text {
                    target,
                    kind: TextKind::Re(build_regex(query, case)?),
                },
            };
            return Ok(Self {
                clauses: vec![Conjunction { leaves: vec![leaf] }],
                match_case: case,
                needs_path: opts.match_path,
            });
        }

        let mut needs_path = false;
        let mut clauses: Vec<Conjunction> = Vec::new();
        let mut current: Vec<Leaf> = Vec::new();
        let mut negate = false;

        for token in tokenize(&opts.query) {
            match token {
                Token::Or => {
                    clauses.push(Conjunction {
                        leaves: std::mem::take(&mut current),
                    });
                    negate = false;
                }
                Token::Not => negate = !negate,
                Token::Word(word) => {
                    if let Some(pred) = parse_word(&word, opts, &mut needs_path)? {
                        current.push(Leaf { negate, pred });
                    }
                    negate = false;
                }
                Token::Phrase(phrase) => {
                    if let Some(pred) = phrase_pred(&phrase, opts, &mut needs_path) {
                        current.push(Leaf { negate, pred });
                    }
                    negate = false;
                }
            }
        }
        clauses.push(Conjunction { leaves: current });
        clauses.retain(|c| !c.leaves.is_empty());

        Ok(Self {
            clauses,
            match_case: case,
            needs_path,
        })
    }

    /// True when nothing constrains the result set (so every entry matches).
    pub fn matches_all(&self) -> bool {
        self.clauses.is_empty()
    }

    /// Whether matching requires the reconstructed full path.
    pub fn needs_path(&self) -> bool {
        self.needs_path
    }

    /// Test one entry. `e.path`/`e.path_lower` may be empty when `needs_path()`
    /// is false (no predicate looks at the path).
    pub fn eval(&self, e: &EntryView) -> bool {
        if self.clauses.is_empty() {
            return true;
        }
        self.clauses.iter().any(|conj| {
            conj.leaves.iter().all(|leaf| {
                let hit = self.eval_pred(&leaf.pred, e);
                hit ^ leaf.negate
            })
        })
    }

    fn eval_pred(&self, pred: &Pred, e: &EntryView) -> bool {
        match pred {
            Pred::File => !e.is_dir,
            Pred::Folder => e.is_dir,
            Pred::Ext(list) => ext_allows(e.name_lower, list),
            Pred::Size(filter) => !e.is_dir && size_within(e.size, filter),
            Pred::Date { field, filter } => {
                let value = match field {
                    DateField::Modified => e.modified_ms,
                    DateField::Created => e.created_ms,
                    DateField::Accessed => e.accessed_ms,
                };
                date_within(value, filter)
            }
            Pred::Attrib(mask) => e.attributes & mask == *mask,
            Pred::Text { target, kind } => {
                let (orig, lower) = match target {
                    TextTarget::Name => (e.name, e.name_lower),
                    TextTarget::Path => (e.path, e.path_lower),
                };
                match kind {
                    TextKind::Plain(needle) => {
                        if self.match_case {
                            orig.contains(needle)
                        } else {
                            lower.contains(needle)
                        }
                    }
                    TextKind::Re(re) => re.is_match(orig),
                }
            }
        }
    }
}

/// Pull every `content:term` / `content:"phrase"` out of `query`, returning the
/// remaining filename/metadata query and the content terms (AND-combined).
///
/// Content matching reads file bodies and so is evaluated separately, in the
/// GUI's user token — never the query-only service — over the candidates the
/// remaining query narrows to. An empty term (a bare `content:`) is dropped.
pub fn extract_content(query: &str) -> (String, Vec<String>) {
    const KW: [char; 8] = ['c', 'o', 'n', 't', 'e', 'n', 't', ':'];
    let chars: Vec<char> = query.chars().collect();
    let mut terms = Vec::new();
    let mut rest = String::with_capacity(query.len());
    let mut i = 0;

    while i < chars.len() {
        let at_boundary = i == 0 || chars[i - 1].is_whitespace();
        let is_kw = at_boundary
            && i + KW.len() <= chars.len()
            && chars[i..i + KW.len()]
                .iter()
                .zip(KW.iter())
                .all(|(a, b)| a.eq_ignore_ascii_case(b));
        if !is_kw {
            rest.push(chars[i]);
            i += 1;
            continue;
        }

        i += KW.len(); // skip "content:"
        let term: String = if chars.get(i) == Some(&'"') {
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            let t = chars[start..i].iter().collect();
            if i < chars.len() {
                i += 1; // closing quote
            }
            t
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            chars[start..i].iter().collect()
        };
        if !term.is_empty() {
            terms.push(term);
        }
    }

    (rest.split_whitespace().collect::<Vec<_>>().join(" "), terms)
}

/// Split a query into words, quoted phrases, and the `|`/`!` operators. `|`
/// and `"` cannot appear in NTFS file names, so they are always operators; `!`
/// is the NOT operator only at the start of a term (so names like `!Locales`
/// are still searchable as substrings, and `"!x"` matches one literally).
fn tokenize(query: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    let mut chars = query.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                flush_word(&mut buf, &mut tokens);
                let mut phrase = String::new();
                for n in chars.by_ref() {
                    if n == '"' {
                        break;
                    }
                    phrase.push(n);
                }
                if !phrase.is_empty() {
                    tokens.push(Token::Phrase(phrase));
                }
            }
            '|' => {
                flush_word(&mut buf, &mut tokens);
                tokens.push(Token::Or);
            }
            '!' if buf.is_empty() => tokens.push(Token::Not),
            c if c.is_whitespace() => flush_word(&mut buf, &mut tokens),
            _ => buf.push(c),
        }
    }
    flush_word(&mut buf, &mut tokens);
    tokens
}

fn flush_word(buf: &mut String, tokens: &mut Vec<Token>) {
    if !buf.is_empty() {
        tokens.push(Token::Word(std::mem::take(buf)));
    }
}

/// Parse a bare word into a predicate: a function (`ext:`/`size:`/`file:`/
/// `folder:`/`path:`) or a text match.
fn parse_word(
    word: &str,
    opts: &SearchOptions,
    needs_path: &mut bool,
) -> Result<Option<Pred>, String> {
    if word.eq_ignore_ascii_case("file:") || word.eq_ignore_ascii_case("files:") {
        return Ok(Some(Pred::File));
    }
    if word.eq_ignore_ascii_case("folder:")
        || word.eq_ignore_ascii_case("folders:")
        || word.eq_ignore_ascii_case("dir:")
    {
        return Ok(Some(Pred::Folder));
    }
    if let Some(v) = word.strip_prefix("ext:") {
        let exts: Vec<String> = v
            .split([';', ','])
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect();
        return Ok(if exts.is_empty() {
            None
        } else {
            Some(Pred::Ext(exts))
        });
    }
    if let Some(v) = word.strip_prefix("size:") {
        return Ok(parse_size_filter(v).map(Pred::Size));
    }
    if let Some(v) = word.strip_prefix("dm:") {
        return Ok(date_pred(v, DateField::Modified));
    }
    if let Some(v) = word.strip_prefix("dc:") {
        return Ok(date_pred(v, DateField::Created));
    }
    if let Some(v) = word.strip_prefix("da:") {
        return Ok(date_pred(v, DateField::Accessed));
    }
    if let Some(v) = word.strip_prefix("attrib:") {
        return Ok(parse_attrib(v).map(Pred::Attrib));
    }
    if let Some(v) = word.strip_prefix("path:") {
        *needs_path = true;
        return Ok(text_pred(v, TextTarget::Path, opts));
    }

    let target = if opts.match_path {
        *needs_path = true;
        TextTarget::Path
    } else {
        TextTarget::Name
    };
    Ok(text_pred(word, target, opts))
}

/// A quoted phrase is always a literal substring (no functions or wildcards).
fn phrase_pred(phrase: &str, opts: &SearchOptions, needs_path: &mut bool) -> Option<Pred> {
    let target = if opts.match_path {
        *needs_path = true;
        TextTarget::Path
    } else {
        TextTarget::Name
    };
    let needle = if opts.match_case {
        phrase.to_string()
    } else {
        phrase.to_lowercase()
    };
    Some(Pred::Text {
        target,
        kind: TextKind::Plain(needle),
    })
}

/// Build a text predicate from `text`, honouring wildcards, whole-word and case.
fn text_pred(text: &str, target: TextTarget, opts: &SearchOptions) -> Option<Pred> {
    if text.is_empty() {
        return None;
    }
    let kind = if text.contains('*') || text.contains('?') {
        match build_regex(&glob_pattern(text), opts.match_case) {
            Ok(re) => TextKind::Re(re),
            Err(_) => return None,
        }
    } else if opts.whole_word {
        match build_regex(&word_pattern(text), opts.match_case) {
            Ok(re) => TextKind::Re(re),
            Err(_) => return None,
        }
    } else if opts.match_case {
        TextKind::Plain(text.to_string())
    } else {
        TextKind::Plain(text.to_lowercase())
    };
    Some(Pred::Text { target, kind })
}

fn ext_allows(name_lower: &str, exts: &[String]) -> bool {
    match name_lower.rfind('.') {
        Some(i) => {
            let ext = &name_lower[i + 1..];
            exts.iter().any(|e| e == ext)
        }
        None => false,
    }
}

fn size_within(size: Option<u64>, filter: &SizeFilter) -> bool {
    let bytes = size.unwrap_or(0);
    if filter.min.is_some_and(|m| bytes < m) {
        return false;
    }
    if filter.max.is_some_and(|m| bytes > m) {
        return false;
    }
    true
}

fn build_regex(pattern: &str, match_case: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(!match_case)
        .size_limit(1 << 22)
        .build()
        .map_err(|e| e.to_string())
}

/// Translate a `*`/`?` wildcard token into an anchored regex pattern.
fn glob_pattern(glob: &str) -> String {
    let mut p = String::from("^");
    let mut literal = String::new();
    for c in glob.chars() {
        if c == '*' || c == '?' {
            if !literal.is_empty() {
                p.push_str(&regex::escape(&literal));
                literal.clear();
            }
            p.push_str(if c == '*' { ".*" } else { "." });
        } else {
            literal.push(c);
        }
    }
    if !literal.is_empty() {
        p.push_str(&regex::escape(&literal));
    }
    p.push('$');
    p
}

fn word_pattern(token: &str) -> String {
    format!(r"\b{}\b", regex::escape(token))
}

/// Parse a `size:` value such as `>100mb`, `<=1gb`, `>=512`, or `4kb`.
fn parse_size_filter(value: &str) -> Option<SizeFilter> {
    let (op, rest) = if let Some(r) = value.strip_prefix(">=") {
        (">=", r)
    } else if let Some(r) = value.strip_prefix("<=") {
        ("<=", r)
    } else if let Some(r) = value.strip_prefix('>') {
        (">", r)
    } else if let Some(r) = value.strip_prefix('<') {
        ("<", r)
    } else {
        ("=", value)
    };

    let bytes = parse_size_bytes(rest)?;
    Some(match op {
        ">" => SizeFilter {
            min: Some(bytes.saturating_add(1)),
            max: None,
        },
        ">=" => SizeFilter {
            min: Some(bytes),
            max: None,
        },
        "<" => SizeFilter {
            min: None,
            max: Some(bytes.saturating_sub(1)),
        },
        "<=" => SizeFilter {
            min: None,
            max: Some(bytes),
        },
        _ => SizeFilter {
            min: Some(bytes),
            max: Some(bytes),
        },
    })
}

/// Parse a number with an optional unit (`kb`/`mb`/`gb`/`tb`, 1024-based).
fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num.parse().ok()?;
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" | "k" => 1024.0,
        "mb" | "m" => 1024.0 * 1024.0,
        "gb" | "g" => 1024.0 * 1024.0 * 1024.0,
        "tb" | "t" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((value * multiplier) as u64)
}

/// Build a date predicate, or `None` if the value is unparseable.
fn date_pred(value: &str, field: DateField) -> Option<Pred> {
    parse_date_filter(value).map(|filter| Pred::Date { field, filter })
}

/// Parse a `dm:`/`dc:`/`da:` value into inclusive millisecond bounds. Accepts a
/// keyword (`today`, `yesterday`, `thisweek`/`lastweek`, `thismonth`/
/// `lastmonth`, `thisyear`/`lastyear`), an absolute `YYYY`, `YYYY-MM` or
/// `YYYY-MM-DD` (`/` also allowed), each optionally prefixed by `>`/`>=`/`<`/
/// `<=`/`=`, or an inclusive `A..B` range.
fn parse_date_filter(value: &str) -> Option<DateFilter> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some((a, b)) = value.split_once("..") {
        let (start, _) = parse_date_span(a)?;
        let (_, end) = parse_date_span(b)?;
        return Some(DateFilter {
            min: Some(start),
            max: Some(end - 1),
        });
    }
    let (op, rest) = split_date_op(value);
    let (start, end) = parse_date_span(rest)?;
    Some(match op {
        ">" => DateFilter {
            min: Some(end),
            max: None,
        },
        ">=" => DateFilter {
            min: Some(start),
            max: None,
        },
        "<" => DateFilter {
            min: None,
            max: Some(start - 1),
        },
        "<=" => DateFilter {
            min: None,
            max: Some(end - 1),
        },
        _ => DateFilter {
            min: Some(start),
            max: Some(end - 1),
        },
    })
}

fn split_date_op(value: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = value.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", value)
}

/// Resolve a date keyword or absolute date into a half-open local-time span
/// `[start, end)` in Unix milliseconds.
fn parse_date_span(s: &str) -> Option<(i64, i64)> {
    let s = s.trim().to_ascii_lowercase();
    let today = Local::now().date_naive();

    match s.as_str() {
        "today" => return span_days(today, today.checked_add_days(Days::new(1))?),
        "yesterday" => return span_days(today.checked_sub_days(Days::new(1))?, today),
        "thisweek" | "lastweek" => {
            let monday =
                today.checked_sub_days(Days::new(today.weekday().num_days_from_monday() as u64))?;
            return if s == "thisweek" {
                span_days(monday, monday.checked_add_days(Days::new(7))?)
            } else {
                span_days(monday.checked_sub_days(Days::new(7))?, monday)
            };
        }
        "thismonth" | "lastmonth" => {
            let first = today.with_day(1)?;
            return if s == "thismonth" {
                span_days(first, first.checked_add_months(Months::new(1))?)
            } else {
                span_days(first.checked_sub_months(Months::new(1))?, first)
            };
        }
        "thisyear" | "lastyear" => {
            let jan1 = NaiveDate::from_ymd_opt(today.year(), 1, 1)?;
            return if s == "thisyear" {
                span_days(jan1, NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?)
            } else {
                span_days(NaiveDate::from_ymd_opt(today.year() - 1, 1, 1)?, jan1)
            };
        }
        _ => {}
    }

    let parts: Vec<&str> = s.split(['-', '/']).collect();
    match parts.as_slice() {
        [y] => {
            let year: i32 = y.parse().ok()?;
            span_days(
                NaiveDate::from_ymd_opt(year, 1, 1)?,
                NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)?,
            )
        }
        [y, m] => {
            let first = NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, 1)?;
            span_days(first, first.checked_add_months(Months::new(1))?)
        }
        [y, m, d] => {
            let date = NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)?;
            span_days(date, date.checked_add_days(Days::new(1))?)
        }
        _ => None,
    }
}

/// Convert a `[start, end)` date range (end exclusive) to Unix milliseconds at
/// local midnight.
fn span_days(start: NaiveDate, end: NaiveDate) -> Option<(i64, i64)> {
    Some((local_midnight_ms(start)?, local_midnight_ms(end)?))
}

/// Unix milliseconds at local-time midnight of `date`.
fn local_midnight_ms(date: NaiveDate) -> Option<i64> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    Some(
        Local
            .from_local_datetime(&naive)
            .earliest()?
            .timestamp_millis(),
    )
}

fn date_within(value: Option<i64>, filter: &DateFilter) -> bool {
    let Some(ms) = value else {
        return false; // unknown dates never satisfy a date filter
    };
    if filter.min.is_some_and(|m| ms < m) {
        return false;
    }
    if filter.max.is_some_and(|m| ms > m) {
        return false;
    }
    true
}

/// Parse an `attrib:` value (a run of attribute letters) into a bitmask that the
/// entry must have fully set. Unknown letters are ignored; an empty mask yields
/// `None` so the predicate is dropped rather than matching everything.
fn parse_attrib(value: &str) -> Option<u32> {
    let mut mask = 0u32;
    for c in value.chars() {
        mask |= match c.to_ascii_lowercase() {
            'r' => 0x0000_0001, // readonly
            'h' => 0x0000_0002, // hidden
            's' => 0x0000_0004, // system
            'd' => 0x0000_0010, // directory
            'a' => 0x0000_0020, // archive
            'n' => 0x0000_0080, // normal
            't' => 0x0000_0100, // temporary
            'p' => 0x0000_0200, // sparse file
            'l' => 0x0000_0400, // reparse point
            'c' => 0x0000_0800, // compressed
            'o' => 0x0000_1000, // offline
            'i' => 0x0000_2000, // not content indexed
            'e' => 0x0000_4000, // encrypted
            'v' => 0x0001_0000, // virtual
            _ => continue,
        };
    }
    (mask != 0).then_some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_content_pulls_terms_and_cleans_query() {
        let (rest, terms) =
            extract_content(r#"*.log content:"error timeout" report content:fatal"#);
        assert_eq!(rest, "*.log report");
        assert_eq!(
            terms,
            vec!["error timeout".to_string(), "fatal".to_string()]
        );

        let (rest, terms) = extract_content("just a name search");
        assert_eq!(rest, "just a name search");
        assert!(terms.is_empty());

        // A bare `content:` (no term) is dropped; `content` as a substring is not
        // a function.
        let (rest, terms) = extract_content("content: mycontent:x report");
        assert_eq!(rest, "mycontent:x report");
        assert!(terms.is_empty());
    }

    fn matcher(query: &str) -> Matcher {
        Matcher::compile(&SearchOptions {
            query: query.into(),
            ..SearchOptions::default()
        })
        .unwrap()
    }

    fn view<'a>(
        name: &'a str,
        name_lower: &'a str,
        is_dir: bool,
        size: Option<u64>,
    ) -> EntryView<'a> {
        EntryView {
            name,
            name_lower,
            path: "",
            path_lower: "",
            is_dir,
            size,
            modified_ms: None,
            created_ms: None,
            accessed_ms: None,
            attributes: 0,
        }
    }

    fn hits(m: &Matcher, name: &str, is_dir: bool, size: Option<u64>) -> bool {
        m.eval(&view(name, &name.to_lowercase(), is_dir, size))
    }

    fn eval_attr(m: &Matcher, attributes: u32) -> bool {
        let mut v = view("x", "x", false, None);
        v.attributes = attributes;
        m.eval(&v)
    }

    fn eval_modified(m: &Matcher, modified_ms: Option<i64>) -> bool {
        let mut v = view("x", "x", false, None);
        v.modified_ms = modified_ms;
        m.eval(&v)
    }

    #[test]
    fn implicit_and() {
        let m = matcher("foo bar");
        assert!(hits(&m, "a-foo-bar.txt", false, None));
        assert!(!hits(&m, "foo.txt", false, None));
    }

    #[test]
    fn or_operator() {
        let m = matcher("foo | bar");
        assert!(hits(&m, "foo.txt", false, None));
        assert!(hits(&m, "bar.txt", false, None));
        assert!(!hits(&m, "baz.txt", false, None));
    }

    #[test]
    fn not_operator() {
        let m = matcher("report !draft");
        assert!(hits(&m, "report-final.txt", false, None));
        assert!(!hits(&m, "report-draft.txt", false, None));
    }

    #[test]
    fn quoted_phrase_is_literal() {
        let m = matcher("\"a | b\"");
        assert!(hits(&m, "x a | b y", false, None));
        assert!(!hits(&m, "a then b", false, None));
    }

    #[test]
    fn functions_combine_with_booleans() {
        let m = matcher("ext:dll | ext:exe");
        assert!(hits(&m, "user32.dll", false, None));
        assert!(hits(&m, "app.exe", false, None));
        assert!(!hits(&m, "notes.txt", false, None));

        let m = matcher("folder: data");
        assert!(hits(&m, "data", true, None));
        assert!(!hits(&m, "data.txt", false, None));

        let m = matcher("size:>1mb");
        assert!(hits(&m, "big.bin", false, Some(2 * 1024 * 1024)));
        assert!(!hits(&m, "small.bin", false, Some(1024)));
    }

    #[test]
    fn empty_matches_all() {
        let m = matcher("");
        assert!(m.matches_all());
        assert!(hits(&m, "anything", false, None));
    }

    #[test]
    fn attrib_requires_all_listed_bits() {
        let m = matcher("attrib:h");
        assert!(eval_attr(&m, 0x2)); // hidden
        assert!(eval_attr(&m, 0x2 | 0x20)); // hidden + archive
        assert!(!eval_attr(&m, 0x20)); // archive only

        let m = matcher("attrib:hs");
        assert!(eval_attr(&m, 0x2 | 0x4)); // hidden + system
        assert!(!eval_attr(&m, 0x2)); // hidden but not system
    }

    #[test]
    fn date_filters_use_inclusive_bounds() {
        // 2024-06-15T12:00:00Z — months from any year boundary, so timezone
        // offset can't move it across the asserted edges.
        let ms = Some(1_718_452_800_000);

        assert!(eval_modified(&matcher("dm:>=2024-01-01"), ms));
        assert!(!eval_modified(&matcher("dm:<2024-01-01"), ms));
        assert!(eval_modified(&matcher("dm:2024-01-01..2024-12-31"), ms));
        assert!(eval_modified(&matcher("dm:2024"), ms));
        assert!(!eval_modified(&matcher("dm:2023"), ms));
    }

    #[test]
    fn unknown_date_never_matches() {
        assert!(!eval_modified(&matcher("dm:>=2000"), None));
    }

    #[test]
    fn out_of_range_year_is_dropped_not_panicking() {
        // Years past chrono's range (and i32::MAX, which would overflow `year+1`)
        // must drop the term rather than panic.
        assert!(matcher("dm:2147483647").matches_all());
        assert!(matcher("dm:>=999999999").matches_all());
        assert!(matcher("dm:2147483647..2147483647").matches_all());
    }

    #[test]
    fn date_keywords_parse() {
        // Relative keywords depend on "now"; assert only that they compile to a
        // usable predicate (not dropped) rather than a specific instant.
        for kw in [
            "today",
            "yesterday",
            "thisweek",
            "lastweek",
            "thismonth",
            "lastmonth",
            "thisyear",
            "lastyear",
        ] {
            let m = matcher(&format!("dm:{kw}"));
            assert!(!m.matches_all(), "dm:{kw} should be a real predicate");
        }
    }
}
