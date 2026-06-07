//! Tauri-managed application state and the background indexing lifecycle.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

use crate::application::{Catalog, IndexStatus, SearchIndex};
use crate::infrastructure::cache;
use crate::infrastructure::ntfs::{
    catch_up, journal_state, ntfs_fixed_drives, volume_serial, CatchUp, MftReader, UsnWatcher,
};

/// How often the on-disk cache is refreshed while the app runs.
const CACHE_SAVE_INTERVAL: Duration = Duration::from_secs(300);

/// Where indexing is in its lifecycle.
enum Phase {
    Indexing,
    Ready,
    Error(String),
}

/// A volume that finished indexing, plus what the cache saver needs.
struct IndexedVolume {
    drive: char,
    serial: u32,
}

/// Shared state managed by Tauri and read by command handlers.
pub struct AppState {
    pub catalog: Arc<RwLock<Catalog>>,
    phase: Arc<RwLock<Phase>>,
    /// Entries committed from volumes already finished.
    committed: Arc<AtomicUsize>,
    /// Running entry count of the volume currently being scanned.
    progress: Arc<AtomicUsize>,
    /// Label of the volume currently being scanned.
    scanning: Arc<RwLock<String>>,
    drives: Arc<RwLock<Vec<char>>>,
    watchers: Arc<Mutex<Vec<UsnWatcher>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            catalog: Arc::new(RwLock::new(Catalog::default())),
            phase: Arc::new(RwLock::new(Phase::Indexing)),
            committed: Arc::new(AtomicUsize::new(0)),
            progress: Arc::new(AtomicUsize::new(0)),
            scanning: Arc::new(RwLock::new(String::new())),
            drives: Arc::new(RwLock::new(Vec::new())),
            watchers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Bring every fixed NTFS volume online on a background thread: load its
    /// cached index and replay journal changes where possible, otherwise scan
    /// the MFT. Each volume starts its USN watcher as it completes.
    pub fn start_indexing(&self) {
        let catalog = self.catalog.clone();
        let phase = self.phase.clone();
        let committed = self.committed.clone();
        let progress = self.progress.clone();
        let scanning = self.scanning.clone();
        let drives_slot = self.drives.clone();
        let watchers = self.watchers.clone();

        thread::spawn(move || {
            let drives = ntfs_fixed_drives();
            *drives_slot.write() = drives.clone();

            if drives.is_empty() {
                *phase.write() = Phase::Error(
                    "no fixed NTFS volumes found — run AllTheThings as Administrator".into(),
                );
                return;
            }

            let mut indexed: Vec<IndexedVolume> = Vec::new();
            let mut last_error = String::new();

            for drive in drives {
                *scanning.write() = format!("{drive}:");
                progress.store(0, Ordering::Relaxed);
                let serial = volume_serial(drive);

                let outcome = bring_volume_online(drive, serial, &progress);
                match outcome {
                    Ok(volume) => {
                        let count = volume.index.len();
                        // Save before publishing so a cache exists immediately.
                        if let Some(next_usn) = volume.resume_usn {
                            let _ = cache::save(
                                drive,
                                serial,
                                volume.journal_id,
                                next_usn,
                                volume.index.export(),
                            );
                        }
                        catalog.write().upsert_volume(volume.index);
                        watchers.lock().push(UsnWatcher::spawn(
                            drive,
                            catalog.clone(),
                            volume.resume_usn,
                        ));
                        committed.fetch_add(count, Ordering::Relaxed);
                        indexed.push(IndexedVolume { drive, serial });
                    }
                    Err(e) => last_error = e,
                }
            }

            *scanning.write() = String::new();
            progress.store(0, Ordering::Relaxed);
            *phase.write() = if indexed.is_empty() {
                Phase::Error(last_error)
            } else {
                Phase::Ready
            };

            if !indexed.is_empty() {
                spawn_cache_saver(catalog, indexed);
            }
        });
    }

    /// Current status for the UI.
    pub fn status(&self) -> IndexStatus {
        let count = self.committed.load(Ordering::Relaxed) + self.progress.load(Ordering::Relaxed);
        match &*self.phase.read() {
            Phase::Indexing => {
                let scanning = self.scanning.read();
                let volume = if scanning.is_empty() {
                    "volumes".to_string()
                } else {
                    scanning.clone()
                };
                IndexStatus::indexing(volume, count)
            }
            Phase::Ready => IndexStatus::ready(self.volume_label(), count),
            Phase::Error(message) => IndexStatus::error(self.volume_label(), message.clone()),
        }
    }

    fn volume_label(&self) -> String {
        let drives = self.drives.read();
        if drives.is_empty() {
            return "—".into();
        }
        drives
            .iter()
            .map(|d| format!("{d}:"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A volume brought online, with the journal position to resume the watcher.
struct OnlineVolume {
    index: SearchIndex,
    journal_id: u64,
    resume_usn: Option<i64>,
}

/// Load a volume from cache and catch it up, or scan its MFT from scratch.
fn bring_volume_online(
    drive: char,
    serial: u32,
    progress: &AtomicUsize,
) -> Result<OnlineVolume, String> {
    // Fast path: a valid cached index we can replay forward.
    if let Some(snapshot) = cache::load(drive) {
        if snapshot.volume_serial == serial {
            let journal_id = snapshot.journal_id;
            let mut index = SearchIndex::import(drive, snapshot.entries);
            progress.store(index.len(), Ordering::Relaxed);
            if let CatchUp::Caught { next_usn } =
                catch_up(drive, journal_id, snapshot.next_usn, &mut index)
            {
                return Ok(OnlineVolume {
                    index,
                    journal_id,
                    resume_usn: Some(next_usn),
                });
            }
        }
    }

    // Full scan. Capture the journal position first so any change during the
    // scan is replayed (idempotently) on the next load rather than lost.
    let journal = journal_state(drive);
    let mut reader = MftReader::open(drive).map_err(|e| e.to_string())?;
    let index = SearchIndex::build_from(&mut reader, progress).map_err(|e| e.to_string())?;

    Ok(OnlineVolume {
        index,
        journal_id: journal.map(|j| j.journal_id).unwrap_or(0),
        resume_usn: journal.map(|j| j.next_usn),
    })
}

/// Periodically refresh each volume's on-disk cache so future startups load
/// fast and only a small journal tail needs replaying.
fn spawn_cache_saver(catalog: Arc<RwLock<Catalog>>, indexed: Vec<IndexedVolume>) {
    thread::spawn(move || loop {
        thread::sleep(CACHE_SAVE_INTERVAL);
        for vol in &indexed {
            // Capture the resume position before exporting, so entries written
            // are at or ahead of it.
            let Some(journal) = journal_state(vol.drive) else {
                continue;
            };
            let entries = catalog.read().volume(vol.drive).map(SearchIndex::export);
            if let Some(entries) = entries {
                let _ = cache::save(
                    vol.drive,
                    vol.serial,
                    journal.journal_id,
                    journal.next_usn,
                    entries,
                );
            }
        }
    });
}
