//! Search options and the compiled query matcher.
//!
//! The query language mirrors voidtools Everything: space-separated terms are
//! AND-combined, `|` is OR, `!` negates the following term, and `"quoted"`
//! text is a literal phrase (escaping the operators). `*`/`?` wildcards,
//! whole-word and regex modes, case sensitivity, full-path matching, and the
//! `ext:`, `path:`, `file:`, `folder:`, `size:` functions are all supported as
//! leaf predicates, so e.g. `ext:dll | ext:exe`, `report !draft`, and
//! `"my file" size:>1mb` all work.

use regex::{Regex, RegexBuilder};
use serde::Deserialize;

/// Which column results are ordered by.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Path,
    Size,
    Modified,
}

/// Everything-style search options sent from the UI.
#[derive(Debug, Clone, Deserialize)]
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
        }
    }
}

/// Inclusive byte-size bounds from a `size:` operator.
#[derive(Default)]
struct SizeFilter {
    min: Option<u64>,
    max: Option<u64>,
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
    Text { target: TextTarget, kind: TextKind },
    Ext(Vec<String>),
    Size(SizeFilter),
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

    /// Test one entry. `path`/`path_lower` may be empty when `needs_path()` is
    /// false (no predicate looks at the path).
    #[allow(clippy::too_many_arguments)]
    pub fn eval(
        &self,
        name: &str,
        name_lower: &str,
        path: &str,
        path_lower: &str,
        is_dir: bool,
        size: Option<u64>,
    ) -> bool {
        if self.clauses.is_empty() {
            return true;
        }
        self.clauses.iter().any(|conj| {
            conj.leaves.iter().all(|leaf| {
                let hit =
                    self.eval_pred(&leaf.pred, name, name_lower, path, path_lower, is_dir, size);
                hit ^ leaf.negate
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_pred(
        &self,
        pred: &Pred,
        name: &str,
        name_lower: &str,
        path: &str,
        path_lower: &str,
        is_dir: bool,
        size: Option<u64>,
    ) -> bool {
        match pred {
            Pred::File => !is_dir,
            Pred::Folder => is_dir,
            Pred::Ext(list) => ext_allows(name_lower, list),
            Pred::Size(filter) => !is_dir && size_within(size, filter),
            Pred::Text { target, kind } => {
                let (orig, lower) = match target {
                    TextTarget::Name => (name, name_lower),
                    TextTarget::Path => (path, path_lower),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(query: &str) -> Matcher {
        Matcher::compile(&SearchOptions {
            query: query.into(),
            ..SearchOptions::default()
        })
        .unwrap()
    }

    fn hits(m: &Matcher, name: &str, is_dir: bool, size: Option<u64>) -> bool {
        m.eval(name, &name.to_lowercase(), "", "", is_dir, size)
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
}
