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
                }
            }
        };

        result.took_ms = start.elapsed().as_millis();
        result
    }
}

/// Re-rank merged hits from multiple volumes by the requested column.
fn sort_hits(hits: &mut [Hit], opts: &SearchOptions) {
    match opts.sort {
        SortKey::Name => hits.sort_by_key(|h| h.name.to_lowercase()),
        SortKey::Path => hits.sort_by_key(|h| h.path.to_lowercase()),
        SortKey::Size => hits.sort_by_key(|h| h.size),
        SortKey::Modified => hits.sort_by_key(|h| h.modified),
        SortKey::Created => hits.sort_by_key(|h| h.created),
        SortKey::Accessed => hits.sort_by_key(|h| h.accessed),
    }
    if !opts.ascending {
        hits.reverse();
    }
}
