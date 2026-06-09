//! Aggregates one [`SearchIndex`] per volume into a single searchable catalog.
//! Each volume keeps its own MFT record numbering and path root; searches fan
//! out across volumes and merge into one globally-ranked result set.

use std::time::Instant;

use rayon::prelude::*;

use crate::application::index::{Hit, SearchIndex, SearchResult};
use crate::application::search::{Matcher, SearchOptions, SortKey};

/// All indexed volumes.
#[derive(Default)]
pub struct Catalog {
    volumes: Vec<SearchIndex>,
}

impl Catalog {
    /// Insert a freshly-built volume index, replacing any prior index for the
    /// same drive.
    pub fn upsert_volume(&mut self, index: SearchIndex) {
        let drive = index.drive();
        match self.volumes.iter_mut().find(|v| v.drive() == drive) {
            Some(slot) => *slot = index,
            None => self.volumes.push(index),
        }
    }

    /// Mutable access to a volume's index (used by its USN watcher).
    pub fn volume_mut(&mut self, drive: char) -> Option<&mut SearchIndex> {
        self.volumes.iter_mut().find(|v| v.drive() == drive)
    }

    /// Shared access to a volume's index (used by the cache saver).
    pub fn volume(&self, drive: char) -> Option<&SearchIndex> {
        self.volumes.iter().find(|v| v.drive() == drive)
    }

    /// Search every volume and return one merged, ranked, truncated result set.
    pub fn search(&self, opts: &SearchOptions) -> SearchResult {
        let start = Instant::now();
        let matcher = match Matcher::compile(opts) {
            Ok(m) => m,
            Err(e) => return SearchResult::error(e, start.elapsed().as_millis()),
        };

        let mut result = match self.volumes.as_slice() {
            [] => SearchResult {
                total: 0,
                took_ms: 0,
                hits: Vec::new(),
                error: None,
                capped: false,
            },
            [only] => only.search(opts, &matcher),
            many => {
                let parts: Vec<SearchResult> =
                    many.par_iter().map(|v| v.search(opts, &matcher)).collect();
                let total = parts.iter().map(|p| p.total).sum();
                let mut hits: Vec<Hit> = parts.into_iter().flat_map(|p| p.hits).collect();
                sort_hits(&mut hits, opts);
                hits.truncate(opts.limit);
                SearchResult {
                    total,
                    took_ms: 0,
                    hits,
                    error: None,
                    capped: false,
                }
            }
        };

        result.took_ms = start.elapsed().as_millis();
        result
    }
}

/// Re-rank merged hits from multiple volumes by the requested column.
fn sort_hits(hits: &mut [Hit], opts: &SearchOptions) {
    if opts.folders_first {
        let asc = opts.ascending;
        let sort = opts.sort;
        // Directories first, then ordered within each group by the column.
        hits.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| ordered(hit_col_cmp(a, b, sort), asc))
        });
        return;
    }
    match opts.sort {
        SortKey::Name => hits.sort_by_key(|h| h.name.to_lowercase()),
        SortKey::Path => hits.sort_by_key(|h| h.path.to_lowercase()),
        SortKey::Size => hits.sort_by_key(|h| h.size),
        SortKey::Modified => hits.sort_by_key(|h| h.modified),
        SortKey::Created => hits.sort_by_key(|h| h.created),
        SortKey::Accessed => hits.sort_by_key(|h| h.accessed),
        SortKey::Ext => hits.sort_by_key(hit_ext),
        SortKey::Attributes => hits.sort_by_key(|h| h.attributes),
    }
    if !opts.ascending {
        hits.reverse();
    }
}

fn ordered(ord: std::cmp::Ordering, ascending: bool) -> std::cmp::Ordering {
    if ascending {
        ord
    } else {
        ord.reverse()
    }
}

/// Lowercased extension of a hit's name (after the last dot), or `""`. A leading
/// dot is a dotfile, not an extension, and directories have no extension —
/// matching the UI's Ext column so the sort agrees with what's displayed.
fn hit_ext(h: &Hit) -> String {
    if h.is_dir {
        return String::new();
    }
    match h.name.rfind('.') {
        Some(i) if i > 0 => h.name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Compare two merged hits by a sort column, ascending.
fn hit_col_cmp(a: &Hit, b: &Hit, sort: SortKey) -> std::cmp::Ordering {
    match sort {
        SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortKey::Path => a.path.to_lowercase().cmp(&b.path.to_lowercase()),
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Modified => a.modified.cmp(&b.modified),
        SortKey::Created => a.created.cmp(&b.created),
        SortKey::Accessed => a.accessed.cmp(&b.accessed),
        SortKey::Ext => hit_ext(a).cmp(&hit_ext(b)),
        SortKey::Attributes => a.attributes.cmp(&b.attributes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, is_dir: bool) -> Hit {
        Hit {
            name: name.into(),
            path: format!("C:\\{name}"),
            size: if is_dir { -1 } else { 1 },
            modified: 0,
            created: 0,
            accessed: 0,
            attributes: 0,
            is_dir,
        }
    }

    fn opts(sort: SortKey, ascending: bool, folders_first: bool) -> SearchOptions {
        SearchOptions {
            sort,
            ascending,
            folders_first,
            ..SearchOptions::default()
        }
    }

    fn names(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.name.as_str()).collect()
    }

    #[test]
    fn folders_first_groups_dirs_then_sorts_within() {
        let mut hits = vec![
            hit("b.txt", false),
            hit("zdir", true),
            hit("a.txt", false),
            hit("adir", true),
        ];
        sort_hits(&mut hits, &opts(SortKey::Name, true, true));
        assert_eq!(names(&hits), ["adir", "zdir", "a.txt", "b.txt"]);
    }

    #[test]
    fn folders_first_keeps_dirs_on_top_when_descending() {
        let mut hits = vec![hit("b.txt", false), hit("adir", true), hit("a.txt", false)];
        sort_hits(&mut hits, &opts(SortKey::Name, false, true));
        // Dirs stay grouped on top; files within the group reverse.
        assert_eq!(names(&hits), ["adir", "b.txt", "a.txt"]);
    }

    #[test]
    fn ext_sort_orders_by_extension() {
        let mut hits = vec![
            hit("a.zip", false),
            hit("b.doc", false),
            hit("c.png", false),
        ];
        sort_hits(&mut hits, &opts(SortKey::Ext, true, false));
        assert_eq!(names(&hits), ["b.doc", "c.png", "a.zip"]);
    }
}
