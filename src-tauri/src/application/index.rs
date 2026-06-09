//! The in-memory file index for a single volume: builds from a
//! [`VolumeEnumerator`], answers queries via a compiled [`Matcher`],
//! reconstructs full paths on demand, and accepts live mutations from the USN
//! change-journal watcher.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

// `Hit`/`SearchResult` are `Serialize` for the UI and `Deserialize` so a service
// client can decode them off the IPC pipe.

use crate::application::error::IndexResult;
use crate::application::indexer::{RawRecord, VolumeEnumerator};
use crate::application::search::{EntryView, Matcher, SearchOptions, SortKey};
use crate::domain::{FileEntry, RecordId};

/// NTFS reserves record 5 for the volume root directory.
const ROOT_RECORD: u64 = 5;
/// Milliseconds between the Windows (1601) and Unix (1970) epochs.
const FILETIME_UNIX_DIFF_MS: i64 = 11_644_473_600_000;
/// Guard against malformed parent cycles when walking to the root. Set far above
/// any real NTFS nesting (a legitimate tree never approaches this) so the cap
/// only ever fires on a corrupt parent cycle — not on a genuinely deep path,
/// which would otherwise be silently truncated to a wrong-but-absolute path.
const MAX_PATH_DEPTH: usize = 4096;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SearchResult {
    pub total: usize,
    #[serde(rename = "tookMs")]
    pub took_ms: u128,
    pub hits: Vec<Hit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The result set was truncated (more matched than were returned/scanned).
    /// Set by content search when the candidate pool hit its cap.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub capped: bool,
}

impl SearchResult {
    pub fn error(message: String, took_ms: u128) -> Self {
        Self {
            took_ms,
            error: Some(message),
            ..Self::default()
        }
    }
}

/// In-memory file index for a single volume.
pub struct SearchIndex {
    entries: Vec<FileEntry>,
    /// MFT record number -> positions in `entries`. A hardlinked file occupies
    /// one MFT record but appears once per name, so a record maps to one position
    /// per link; the live update/delete paths must touch every link, not one.
    by_record: HashMap<u64, Vec<u32>>,
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
        let mut by_record: HashMap<u64, Vec<u32>> = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_record.entry(e.record.0).or_default().push(i as u32);
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
                capped: false,
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
            capped: false,
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
        // Key on a case-folded path so ordering matches the case-insensitive Name
        // column (and Everything/Explorer); keep the original-cased path to
        // display. Fold once per entry, not per comparison.
        let mut keyed: Vec<(u32, String, String)> = matched
            .par_iter()
            .map(|&i| {
                let path = self.build_path(i as usize);
                let lower = path.to_lowercase();
                (i, path, lower)
            })
            .collect();
        if opts.folders_first {
            let asc = opts.ascending;
            keyed.par_sort_unstable_by(|a, b| {
                let (ea, eb) = (&self.entries[a.0 as usize], &self.entries[b.0 as usize]);
                dir_first(ea, eb).then_with(|| ordered(a.2.cmp(&b.2), asc))
            });
        } else {
            keyed.par_sort_unstable_by(|a, b| a.2.cmp(&b.2));
            if !opts.ascending {
                keyed.reverse();
            }
        }
        keyed
            .into_iter()
            .take(opts.limit)
            .map(|(i, path, _)| self.to_hit_with_path(i as usize, path))
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

    /// Insert or replace an entry (USN create / rename / metadata change).
    pub fn apply_upsert(&mut self, r: RawRecord) {
        let Some(entry) = raw_to_entry(r) else {
            return;
        };
        let record = entry.record.0;
        let target = match self.by_record.get(&record) {
            // Unknown record — a new file or the first link.
            None => None,
            // Single-link file: replace that one row in place. A USN event can't
            // say which old (parent, name) it replaced, but for a sole link a
            // rename, a move, and a metadata change all update the same row — so
            // match by record only (preserving the pre-hardlink behaviour).
            Some(positions) if positions.len() == 1 => Some(positions[0]),
            // Hardlinked record: the event names one link, so match it by parent
            // + name and replace that link; an unmatched name is a newly-added
            // hardlink, appended rather than clobbering a sibling.
            Some(positions) => positions.iter().copied().find(|&p| {
                let e = &self.entries[p as usize];
                e.parent.0 == entry.parent.0 && e.name == entry.name
            }),
        };
        match target {
            Some(idx) => self.entries[idx as usize] = entry,
            None => {
                let idx = self.entries.len() as u32;
                self.entries.push(entry);
                self.by_record.entry(record).or_default().push(idx);
            }
        }
        self.name_order_stale = true;
    }

    /// Remove an entry (USN delete). Removes **every** link of the record (a
    /// hardlinked file frees its single MFT record on full deletion), repairing
    /// `by_record` for each entry relocated by `swap_remove`.
    pub fn apply_delete(&mut self, record_no: u64) {
        let Some(mut positions) = self.by_record.remove(&record_no) else {
            return;
        };
        // Remove highest index first so a swap_remove never relocates a position
        // still queued for removal: the entry moved in from the end is always
        // another record's (its index exceeds every remaining position to remove).
        positions.sort_unstable_by(|a, b| b.cmp(a));
        for pos in positions {
            let pos = pos as usize;
            let last = self.entries.len() - 1;
            self.entries.swap_remove(pos);
            if pos != last {
                let moved = self.entries[pos].record.0;
                if let Some(v) = self.by_record.get_mut(&moved) {
                    for p in v.iter_mut() {
                        if *p == last as u32 {
                            *p = pos as u32;
                        }
                    }
                }
            }
        }
        self.name_order_stale = true;
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
            // Any link of the parent record yields the same directory name.
            match self.by_record.get(&parent).and_then(|v| v.first()) {
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
/// A leading dot is a dotfile (`.gitignore`), not an extension — matching what
/// the UI's Ext column displays.
fn ext_of(name_lower: &str) -> &str {
    match name_lower.rfind('.') {
        Some(i) if i > 0 => &name_lower[i + 1..],
        _ => "",
    }
}

/// Ext sort key: blank for directories (whose Ext cell renders empty).
fn dir_ext(e: &FileEntry) -> &str {
    if e.is_dir {
        ""
    } else {
        ext_of(&e.name_lower)
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
        // Directories show a blank Ext cell in the UI, so sort them as such.
        SortKey::Ext => dir_ext(a).cmp(dir_ext(b)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(record: u64, parent: u64, name: &str) -> EntrySnapshot {
        EntrySnapshot {
            record,
            parent,
            name: name.into(),
            is_dir: false,
            size: Some(1),
            modified_ms: None,
            created_ms: None,
            accessed_ms: None,
            attributes: 0,
        }
    }

    fn raw(record: u64, parent: u64, name: &str) -> RawRecord {
        RawRecord {
            record_no: record,
            parent_no: parent,
            name: name.into(),
            is_dir: false,
            size: Some(1),
            modified_ft: 0,
            created_ft: 0,
            accessed_ft: 0,
            attributes: 0,
        }
    }

    /// Every entry is reachable through `by_record`, and no slot is stale.
    fn assert_by_record_consistent(idx: &SearchIndex) {
        for (i, e) in idx.entries.iter().enumerate() {
            let positions = idx.by_record.get(&e.record.0).expect("record is mapped");
            assert!(
                positions.contains(&(i as u32)),
                "entry {i} (record {}) missing from by_record",
                e.record.0
            );
        }
        for (rec, positions) in &idx.by_record {
            for &p in positions {
                assert_eq!(
                    idx.entries[p as usize].record.0, *rec,
                    "stale by_record slot"
                );
            }
        }
    }

    #[test]
    fn delete_removes_all_hardlinks_of_a_record() {
        // Record 50 is hardlinked (two names); record 60 is unrelated.
        let mut idx = SearchIndex::import(
            'C',
            vec![
                snap(50, 5, "a.dll"),
                snap(50, 6, "b.dll"),
                snap(60, 5, "x.txt"),
            ],
        );
        assert_eq!(idx.entries.len(), 3);
        idx.apply_delete(50);
        assert_eq!(idx.entries.len(), 1, "both links of record 50 must be gone");
        assert_eq!(idx.entries[0].record.0, 60);
        assert!(!idx.by_record.contains_key(&50));
        assert_by_record_consistent(&idx);
    }

    #[test]
    fn upsert_targets_the_named_link_and_appends_new_ones() {
        let mut idx = SearchIndex::import('C', vec![snap(50, 5, "a.dll"), snap(50, 6, "b.dll")]);
        // Updating an existing link (record + parent + name) replaces it, never a sibling.
        idx.apply_upsert(raw(50, 6, "b.dll"));
        assert_eq!(idx.entries.len(), 2);
        // A new hardlink to the same record appends a row rather than clobbering one.
        idx.apply_upsert(raw(50, 7, "c.dll"));
        assert_eq!(idx.entries.len(), 3);
        assert_eq!(idx.by_record.get(&50).map(Vec::len), Some(3));
        let names: Vec<&str> = idx.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(["a.dll", "b.dll", "c.dll"]
            .iter()
            .all(|n| names.contains(n)));
        assert_by_record_consistent(&idx);
    }

    #[test]
    fn upsert_renames_single_link_in_place() {
        // A single-link file's rename / move updates its row, never a duplicate.
        let mut idx =
            SearchIndex::import('C', vec![snap(50, 5, "foo.txt"), snap(60, 5, "other.txt")]);
        idx.apply_upsert(raw(50, 5, "bar.txt")); // rename in place
        assert_eq!(idx.entries.len(), 2, "rename must not leave a ghost row");
        let r50 = idx.by_record.get(&50).expect("record 50");
        assert_eq!(r50.len(), 1);
        assert_eq!(idx.entries[r50[0] as usize].name, "bar.txt");
        idx.apply_upsert(raw(50, 7, "bar.txt")); // move to a new parent
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.by_record.get(&50).map(Vec::len), Some(1));
        assert_eq!(idx.entries[idx.by_record[&50][0] as usize].parent.0, 7);
        assert_by_record_consistent(&idx);
    }

    #[test]
    fn delete_repairs_by_record_for_swap_moved_entries() {
        let snaps: Vec<EntrySnapshot> =
            (10..20).map(|r| snap(r, 5, &format!("f{r}.txt"))).collect();
        let mut idx = SearchIndex::import('C', snaps);
        idx.apply_delete(12);
        idx.apply_delete(18);
        idx.apply_delete(10);
        assert_eq!(idx.entries.len(), 7);
        assert_by_record_consistent(&idx);
    }
}
