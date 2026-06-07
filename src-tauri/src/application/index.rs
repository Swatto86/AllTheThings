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
use crate::application::search::{Matcher, SearchOptions, SortKey};
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
}

/// One search result row, serialized to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub name: String,
    pub path: String,
    /// Size in bytes; `-1` for directories or unknown.
    pub size: i64,
    /// Unix milliseconds; `0` when unknown.
    pub modified: i64,
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
            entries[a as usize].name_lower.cmp(&entries[b as usize].name_lower)
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
        if !matcher.kind_allows(e.is_dir)
            || !matcher.size_allows(e.size, e.is_dir)
            || !matcher.ext_allows(&e.name_lower)
        {
            return false;
        }
        if matcher.needs_path() {
            let path = self.build_path(idx);
            let lower = path.to_lowercase();
            matcher.text_allows(&path, &lower)
        } else {
            matcher.text_allows(&e.name, &e.name_lower)
        }
    }

    /// Sort the matched set per `opts` and materialize the top `limit` hits.
    fn rank_and_collect(&self, matched: &mut [u32], opts: &SearchOptions) -> Vec<Hit> {
        match opts.sort {
            SortKey::Path => {
                // Path keys must be built for every match to rank correctly.
                let mut keyed: Vec<(u32, String)> =
                    matched.par_iter().map(|&i| (i, self.build_path(i as usize))).collect();
                keyed.par_sort_unstable_by(|a, b| a.1.cmp(&b.1));
                if !opts.ascending {
                    keyed.reverse();
                }
                keyed
                    .into_iter()
                    .take(opts.limit)
                    .map(|(i, path)| self.to_hit_with_path(i as usize, path))
                    .collect()
            }
            SortKey::Name => {
                matched.par_sort_unstable_by(|&a, &b| {
                    self.entries[a as usize]
                        .name_lower
                        .cmp(&self.entries[b as usize].name_lower)
                });
                self.collect_ranked(matched, opts.ascending, opts.limit)
            }
            SortKey::Size => {
                matched.par_sort_unstable_by_key(|&i| size_key(&self.entries[i as usize]));
                self.collect_ranked(matched, opts.ascending, opts.limit)
            }
            SortKey::Modified => {
                matched.par_sort_unstable_by_key(|&i| self.entries[i as usize].modified_ms.unwrap_or(0));
                self.collect_ranked(matched, opts.ascending, opts.limit)
            }
        }
    }

    fn collect_ranked(&self, matched: &[u32], ascending: bool, limit: usize) -> Vec<Hit> {
        if ascending {
            matched.iter().take(limit).map(|&i| self.to_hit(i as usize)).collect()
        } else {
            matched.iter().rev().take(limit).map(|&i| self.to_hit(i as usize)).collect()
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
    })
}
