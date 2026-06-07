//! Search options and the compiled query matcher.
//!
//! The matcher supports plain substring AND-terms (the fast default), `*`/`?`
//! wildcards, whole-word and regex modes, case sensitivity, matching against
//! the full path, and the `ext:`, `path:`, `file:`/`folder:` operators —
//! covering voidtools Everything's everyday search syntax.

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

/// Restrict results to files, folders, or either.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Any,
    File,
    Folder,
}

/// One AND-combined text predicate over the haystack.
enum Term {
    /// Substring; pre-cased to match the haystack casing.
    Plain(String),
    /// Compiled wildcard / whole-word / regex pattern (case flag baked in).
    Re(Regex),
}

/// Inclusive byte-size bounds from a `size:` operator.
#[derive(Default)]
struct SizeFilter {
    min: Option<u64>,
    max: Option<u64>,
}

/// A compiled query, ready to test against entries.
pub struct Matcher {
    text: Vec<Term>,
    exts: Vec<String>,
    kind: Kind,
    size: Option<SizeFilter>,
    match_case: bool,
    needs_path: bool,
}

impl Matcher {
    /// Compile search options into a matcher, or return a user-facing error
    /// (e.g. an invalid regex).
    pub fn compile(opts: &SearchOptions) -> Result<Self, String> {
        let mut text = Vec::new();
        let mut exts = Vec::new();
        let mut kind = Kind::Any;
        let mut size: Option<SizeFilter> = None;
        let mut needs_path = opts.match_path;
        let case = opts.match_case;

        if opts.regex {
            let q = opts.query.trim();
            if !q.is_empty() {
                text.push(Term::Re(build_regex(q, case)?));
            }
        } else {
            for tok in opts.query.split_whitespace() {
                if let Some(v) = tok.strip_prefix("ext:") {
                    exts.extend(
                        v.split([';', ','])
                            .filter(|e| !e.is_empty())
                            .map(|e| e.to_ascii_lowercase()),
                    );
                    continue;
                }
                if tok.eq_ignore_ascii_case("file:") || tok.eq_ignore_ascii_case("files:") {
                    kind = Kind::File;
                    continue;
                }
                if tok.eq_ignore_ascii_case("folder:")
                    || tok.eq_ignore_ascii_case("folders:")
                    || tok.eq_ignore_ascii_case("dir:")
                {
                    kind = Kind::Folder;
                    continue;
                }
                if let Some(v) = tok.strip_prefix("size:") {
                    if let Some(filter) = parse_size_filter(v) {
                        // AND-combine bounds so a range like
                        // `size:>=1mb size:<16mb` narrows correctly.
                        let merged = size.get_or_insert(SizeFilter::default());
                        if let Some(min) = filter.min {
                            merged.min = Some(merged.min.map_or(min, |cur| cur.max(min)));
                        }
                        if let Some(max) = filter.max {
                            merged.max = Some(merged.max.map_or(max, |cur| cur.min(max)));
                        }
                    }
                    continue;
                }

                let token = if let Some(v) = tok.strip_prefix("path:") {
                    needs_path = true;
                    v
                } else {
                    tok
                };
                if token.is_empty() {
                    continue;
                }

                if token.contains('*') || token.contains('?') {
                    text.push(Term::Re(build_regex(&glob_pattern(token), case)?));
                } else if opts.whole_word {
                    text.push(Term::Re(build_regex(&word_pattern(token), case)?));
                } else if case {
                    text.push(Term::Plain(token.to_string()));
                } else {
                    text.push(Term::Plain(token.to_lowercase()));
                }
            }
        }

        Ok(Self {
            text,
            exts,
            kind,
            size,
            match_case: case,
            needs_path,
        })
    }

    /// True when nothing constrains the result set (so every entry matches).
    pub fn matches_all(&self) -> bool {
        self.text.is_empty()
            && self.exts.is_empty()
            && self.size.is_none()
            && matches!(self.kind, Kind::Any)
    }

    /// Check the `size:` filter. A size constraint implies files only.
    pub fn size_allows(&self, size: Option<u64>, is_dir: bool) -> bool {
        let Some(filter) = &self.size else {
            return true;
        };
        if is_dir {
            return false;
        }
        let bytes = size.unwrap_or(0);
        if filter.min.is_some_and(|m| bytes < m) {
            return false;
        }
        if filter.max.is_some_and(|m| bytes > m) {
            return false;
        }
        true
    }

    /// Whether matching requires the reconstructed full path.
    pub fn needs_path(&self) -> bool {
        self.needs_path
    }

    pub fn kind_allows(&self, is_dir: bool) -> bool {
        match self.kind {
            Kind::Any => true,
            Kind::File => !is_dir,
            Kind::Folder => is_dir,
        }
    }

    /// Check the extension filter against an already-lowercased name.
    pub fn ext_allows(&self, name_lower: &str) -> bool {
        if self.exts.is_empty() {
            return true;
        }
        match name_lower.rfind('.') {
            Some(i) => {
                let ext = &name_lower[i + 1..];
                self.exts.iter().any(|e| e == ext)
            }
            None => false,
        }
    }

    /// Check the text predicates. `orig` is the original-case haystack; `lower`
    /// is its lowercase form (precomputed for names, built once for paths).
    pub fn text_allows(&self, orig: &str, lower: &str) -> bool {
        self.text.iter().all(|t| match t {
            Term::Plain(s) => {
                if self.match_case {
                    orig.contains(s)
                } else {
                    lower.contains(s)
                }
            }
            Term::Re(re) => re.is_match(orig),
        })
    }
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
