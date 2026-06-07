//! The in-memory file index for a single volume: builds from a
//! [`VolumeEnumerator`], answers queries via a compiled [`Matcher`],
//! reconstructs full paths on demand, and accepts live mutations from the USN
//! change-journal watcher.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::application::error::IndexResult;
use crate::application::indexer::{RawRecord, VolumeEnumerator};
use crate::application::search::{EntryView, Matcher, SearchOptions, SortKey};
use crate::domain::{FileEntry, RecordId};

/// NTFS reserves record 5 for the volume root directory.
const ROOT_RECORD: u64 = 5;
/// Milliseconds between the Windows (1601) and Unix (1970) epochs.
const FILETIME_UNIX_DIFF_MS: i64 = 11_644_473_600_000;
/// Guard against malformed parent cycles when walking to the root.
const MAX_PATH_DEPTH: usize = 256;

fn filetime_to_unix_ms(ft: u64) -> Option<i64> {
    if ft == 0 {
        None
    } else {
        Some((ft / 10_000) as i64 - FILETIME_UNIX_DIFF_MS)
    }
}

/// A single entry as stored in the on-disk index cache. `name_lower` and the
/// lookup structures are recomputed on load, so they are not persisted.
#[derive(Debug, Serialize, Deserialize)]
pub struct EntrySnapshot {
    pub record: u64,
    pub parent: u64,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified_ms: Option<i64>,
    pub created_ms: Option<i64>,
    pub accessed_ms: Option<i64>,
    pub attributes: u32,
}

/// One search result row, serialized to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub name: String,
    pub path: String,
    /// Size in bytes; `-1` for directories or unknown.
    pub size: i64,
    /// Last-modified Unix milliseconds; `0` when unknown.
    pub modified: i64,
    /// Creation Unix milliseconds; `0` when unknown.
    pub created: i64,
    /// Last-accessed Unix milliseconds; `0` when unknown.
    pub accessed: i64,
    /// Windows DOS file attributes bitmask.
    pub attributes: u32,
    #[serde(rename = "isDir")]
    pub is_dir: bool,
}

/// The full response for one search.
#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub total: usize,
    #[serde(rename = "tookMs")]
    pub took_ms: u128,
    pub hits: Vec<Hit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SearchResult {
    pub fn error(message: String, took_ms: u128) -> Self {
        Self {
            total: 0,
            took_ms,
            hits: Vec::new(),
            error: Some(message),
        }
    }
}

/// In-memory file index for a single volume.
pub struct SearchIndex {
    entries: Vec<FileEntry>,
    /// MFT record number -> position in `entries`.
    by_record: HashMap<u64, u32>,
    /// Entry positions sorted by `name_lower`, for the instant default view.
    by_name: Vec<u32>,
    /// Set once live mutations make `by_name` stale.
    name_order_stale: bool,
    drive: char,
}

impl SearchIndex {
    /// Build the index by streaming every record from `enumerator`, publishing
    /// the running entry count into `progress` for the status bar.
    pub fn build_from<E: VolumeEnumerator>(
        enumerator: &mut E,
        progress: &AtomicUsize,
    ) -> IndexResult<Self> {
        let drive = enumerator.drive();

        let mut entries: Vec<FileEntry> = Vec::with_capacity(1 << 19);
        enumerator.enumerate(&mut |r: RawRecord| {
            if let Some(entry) = raw_to_entry(r) {
                entries.push(entry);
                if entries.len() & 0x3FFF == 0 {
                    progress.store(entries.len(), Ordering::Relaxed);
                }
            }
        })?;
        progress.store(entries.len(), Ordering::Relaxed);
        Ok(Self::finalize(entries, drive))
    }

    /// Rebuild the lookup map and the sorted name view from a set of entries.
    fn finalize(entries: Vec<FileEntry>, drive: char) -> Self {
        let mut by_record = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_record.insert(e.record.0, i as u32);
        }

        let mut by_name: Vec<u32> = (0..entries.len() as u32).collect();
        by_name.par_sort_unstable_by(|&a, &b| {
            entries[a as usize]
                .name_lower
                .cmp(&entries[b as usize].name_lower)
        });

        Self {
            entries,
            by_record,
            by_name,
            name_order_stale: false,
            drive,
        }
    }

    /// Reconstruct an index from a persisted snapshot (no MFT read).
    pub fn import(drive: char, snapshot: Vec<EntrySnapshot>) -> Self {
        let entries = snapshot
            .into_iter()
            .map(|s| FileEntry {
                record: RecordId(s.record),
                parent: RecordId(s.parent),
                name_lower: s.name.to_lowercase(),
                name: s.name,
                is_dir: s.is_dir,
                size: s.size,
                modified_ms: s.modified_ms,
                created_ms: s.created_ms,
                accessed_ms: s.accessed_ms,
                attributes: s.attributes,
            })
            .collect();
        Self::finalize(entries, drive)
    }

    /// Export entries for persistence to the on-disk cache.
    pub fn export(&self) -> Vec<EntrySnapshot> {
        self.entries
            .iter()
            .map(|e| EntrySnapshot {
                record: e.record.0,
                parent: e.parent.0,
                name: e.name.clone(),
                is_dir: e.is_dir,
                size: e.size,
                modified_ms: e.modified_ms,
                created_ms: e.created_ms,
                accessed_ms: e.accessed_ms,
                attributes: e.attributes,
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn drive(&self) -> char {
        self.drive
    }

    /// Run a compiled search and return up to `opts.limit` rows with full paths.
    pub fn search(&self, opts: &SearchOptions, matcher: &Matcher) -> SearchResult {
        // Fast path: no constraints, default name-ascending order — just slice
        // the precomputed sorted view without touching every entry.
        if matcher.matches_all()
            && matches!(opts.sort, SortKey::Name)
            && opts.ascending
            && !opts.folders_first
            && !self.name_order_stale
        {
            let hits = self
                .by_name
                .iter()
                .take(opts.limit)
                .map(|&i| self.to_hit(i as usize))
                .collect();
            return SearchResult {
                total: self.entries.len(),
                took_ms: 0,
                hits,
                error: None,
            };
        }

        let mut matched: Vec<u32> = if matcher.matches_all() {
            (0..self.entries.len() as u32).collect()
        } else {
            (0..self.entries.len())
                .into_par_iter()
                .filter(|&i| self.entry_matches(i, matcher))
                .map(|i| i as u32)
                .collect()
        };
        let total = matched.len();

        let hits = self.rank_and_collect(&mut matched, opts);
        SearchResult {
            total,
            took_ms: 0,
            hits,
            error: None,
        }
    }

    fn entry_matches(&self, idx: usize, matcher: &Matcher) -> bool {
        let e = &self.entries[idx];
        if matcher.needs_path() {
            let path = self.build_path(idx);
            let lower = path.to_lowercase();
            matcher.eval(&entry_view(e, &path, &lower))
        } else {
            matcher.eval(&entry_view(e, "", ""))
        }
    }

    /// Sort the matched set per `opts` and materialize the top `limit` hits.
    fn rank_and_collect(&self, matched: &mut [u32], opts: &SearchOptions) -> Vec<Hit> {
        // Path ranking needs a reconstructed key per entry, so it is separate.
        if matches!(opts.sort, SortKey::Path) {
            return self.rank_by_path(matched, opts);
        }

        let sort = opts.sort;
        if opts.folders_first {
            // Group directories first, then order within each group by the column.
            let asc = opts.ascending;
            matched.par_sort_unstable_by(|&a, &b| {
                let (ea, eb) = (&self.entries[a as usize], &self.entries[b as usize]);
                dir_first(ea, eb).then_with(|| ordered(col_cmp(ea, eb, sort), asc))
            });
            matched
                .iter()
                .take(opts.limit)
                .map(|&i| self.to_hit(i as usize))
                .collect()
        } else {
            // Sort ascending once; descending just collects from the other end.
            matched.par_sort_unstable_by(|&a, &b| {
                col_cmp(&self.entries[a as usize], &self.entries[b as usize], sort)
            });
            self.collect_ranked(matched, opts.ascending, opts.limit)
        }
    }

    /// Rank by reconstructed full path (built once per match), honouring the
    /// folders-first grouping and sort direction.
    fn rank_by_path(&self, matched: &mut [u32], opts: &SearchOptions) -> Vec<Hit> {
        let mut keyed: Vec<(u32, String)> = matched
            .par_iter()
            .map(|&i| (i, self.build_path(i as usize)))
            .collect();
        if opts.folders_first {
            let asc = opts.ascending;
            keyed.par_sort_unstable_by(|a, b| {
                let (ea, eb) = (&self.entries[a.0 as usize], &self.entries[b.0 as usize]);
                dir_first(ea, eb).then_with(|| ordered(a.1.cmp(&b.1), asc))
            });
        } else {
            keyed.par_sort_unstable_by(|a, b| a.1.cmp(&b.1));
            if !opts.ascending {
                keyed.reverse();
            }
        }
        keyed
            .into_iter()
            .take(opts.limit)
            .map(|(i, path)| self.to_hit_with_path(i as usize, path))
            .collect()
    }

    fn collect_ranked(&self, matched: &[u32], ascending: bool, limit: usize) -> Vec<Hit> {
        if ascending {
            matched
                .iter()
                .take(limit)
                .map(|&i| self.to_hit(i as usize))
                .collect()
        } else {
            matched
                .iter()
                .rev()
                .take(limit)
                .map(|&i| self.to_hit(i as usize))
                .collect()
        }
    }

    /// Insert or replace an entry (USN create / rename).
    pub fn apply_upsert(&mut self, r: RawRecord) {
        let Some(entry) = raw_to_entry(r) else {
            return;
        };
        let record = entry.record.0;
        if let Some(&idx) = self.by_record.get(&record) {
            self.entries[idx as usize] = entry;
        } else {
            let idx = self.entries.len() as u32;
            self.entries.push(entry);
            self.by_record.insert(record, idx);
        }
        self.name_order_stale = true;
    }

    /// Remove an entry (USN delete).
    pub fn apply_delete(&mut self, record_no: u64) {
        if let Some(idx) = self.by_record.remove(&record_no) {
            let last = self.entries.len() - 1;
            self.entries.swap_remove(idx as usize);
            if (idx as usize) != last {
                let moved = self.entries[idx as usize].record.0;
                self.by_record.insert(moved, idx);
            }
            self.name_order_stale = true;
        }
    }

    fn to_hit(&self, idx: usize) -> Hit {
        self.to_hit_with_path(idx, self.build_path(idx))
    }

    fn to_hit_with_path(&self, idx: usize, path: String) -> Hit {
        let e = &self.entries[idx];
        Hit {
            name: e.name.clone(),
            path,
            size: size_key(e),
            modified: e.modified_ms.unwrap_or(0),
            created: e.created_ms.unwrap_or(0),
            accessed: e.accessed_ms.unwrap_or(0),
            attributes: e.attributes,
            is_dir: e.is_dir,
        }
    }

    /// Reconstruct an entry's absolute path by walking parents to the root.
    fn build_path(&self, idx: usize) -> String {
        let e = &self.entries[idx];
        let mut parts: Vec<&str> = vec![e.name.as_str()];

        let mut parent = e.parent.0;
        let mut depth = 0;
        while parent != ROOT_RECORD && depth < MAX_PATH_DEPTH {
            match self.by_record.get(&parent) {
                Some(&pi) => {
                    let pe = &self.entries[pi as usize];
                    parts.push(pe.name.as_str());
                    parent = pe.parent.0;
                }
                None => break,
            }
            depth += 1;
        }

        let mut path = String::with_capacity(64);
        path.push(self.drive);
        path.push(':');
        for part in parts.iter().rev() {
            path.push('\\');
            path.push_str(part);
        }
        path
    }
}

/// Size as a sort/display key: `-1` for directories or unknown size.
fn size_key(e: &FileEntry) -> i64 {
    if e.is_dir {
        -1
    } else {
        e.size.map(|s| s as i64).unwrap_or(-1)
    }
}

/// Directories before files — the folders-first primary sort key.
fn dir_first(a: &FileEntry, b: &FileEntry) -> std::cmp::Ordering {
    b.is_dir.cmp(&a.is_dir)
}

/// Apply the sort direction to a comparison result.
fn ordered(ord: std::cmp::Ordering, ascending: bool) -> std::cmp::Ordering {
    if ascending {
        ord
    } else {
        ord.reverse()
    }
}

/// Extension slice of a lowercased name (after the last dot), or `""` if none.
fn ext_of(name_lower: &str) -> &str {
    match name_lower.rfind('.') {
        Some(i) => &name_lower[i + 1..],
        None => "",
    }
}

/// Compare two entries by a sort column, ascending. `Path` is ranked separately.
fn col_cmp(a: &FileEntry, b: &FileEntry, sort: SortKey) -> std::cmp::Ordering {
    match sort {
        SortKey::Name => a.name_lower.cmp(&b.name_lower),
        SortKey::Size => size_key(a).cmp(&size_key(b)),
        SortKey::Modified => a.modified_ms.unwrap_or(0).cmp(&b.modified_ms.unwrap_or(0)),
        SortKey::Created => a.created_ms.unwrap_or(0).cmp(&b.created_ms.unwrap_or(0)),
        SortKey::Accessed => a.accessed_ms.unwrap_or(0).cmp(&b.accessed_ms.unwrap_or(0)),
        SortKey::Ext => ext_of(&a.name_lower).cmp(ext_of(&b.name_lower)),
        SortKey::Attributes => a.attributes.cmp(&b.attributes),
        SortKey::Path => std::cmp::Ordering::Equal,
    }
}

fn raw_to_entry(r: RawRecord) -> Option<FileEntry> {
    // The root record's name is "." — never a useful search hit.
    if r.name.is_empty() || r.record_no == ROOT_RECORD {
        return None;
    }
    Some(FileEntry {
        record: RecordId(r.record_no),
        parent: RecordId(r.parent_no),
        name_lower: r.name.to_lowercase(),
        name: r.name,
        is_dir: r.is_dir,
        size: r.size,
        modified_ms: filetime_to_unix_ms(r.modified_ft),
        created_ms: filetime_to_unix_ms(r.created_ft),
        accessed_ms: filetime_to_unix_ms(r.accessed_ft),
        attributes: r.attributes,
    })
}

/// Borrow an entry's fields into the matcher's evaluation view. `path` /
/// `path_lower` may be empty when no predicate needs the full path.
fn entry_view<'a>(e: &'a FileEntry, path: &'a str, path_lower: &'a str) -> EntryView<'a> {
    EntryView {
        name: &e.name,
        name_lower: &e.name_lower,
        path,
        path_lower,
        is_dir: e.is_dir,
        size: e.size,
        modified_ms: e.modified_ms,
        created_ms: e.created_ms,
        accessed_ms: e.accessed_ms,
        attributes: e.attributes,
    }
}
