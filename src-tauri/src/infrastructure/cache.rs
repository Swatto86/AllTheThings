//! On-disk index cache. Each volume's index is persisted with the USN journal
//! position captured before its scan, so a later run can load instantly and
//! replay only the changes since.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::application::EntrySnapshot;

/// Bumped whenever the on-disk layout changes; mismatches are ignored on load.
/// v2 added per-entry creation/access times and DOS attributes.
const FORMAT_VERSION: u32 = 2;

/// A persisted per-volume index plus its journal resume point.
#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub format_version: u32,
    pub drive: char,
    pub volume_serial: u32,
    pub journal_id: u64,
    pub next_usn: i64,
    pub entries: Vec<EntrySnapshot>,
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("AllTheThings").join("cache"))
}

/// Load a volume's cached snapshot, or `None` if absent/unreadable/incompatible.
pub fn load(drive: char) -> Option<Snapshot> {
    let path = cache_dir()?.join(format!("{drive}.idx"));
    let file = File::open(path).ok()?;
    let snapshot: Snapshot = bincode::deserialize_from(BufReader::new(file)).ok()?;
    if snapshot.format_version != FORMAT_VERSION || snapshot.drive != drive {
        return None;
    }
    Some(snapshot)
}

/// Persist a volume snapshot, replacing any previous one atomically.
pub fn save(
    drive: char,
    volume_serial: u32,
    journal_id: u64,
    next_usn: i64,
    entries: Vec<EntrySnapshot>,
) -> io::Result<()> {
    let snapshot = Snapshot {
        format_version: FORMAT_VERSION,
        drive,
        volume_serial,
        journal_id,
        next_usn,
        entries,
    };

    let dir = cache_dir().ok_or_else(|| io::Error::other("LOCALAPPDATA not set"))?;
    fs::create_dir_all(&dir)?;

    let final_path = dir.join(format!("{drive}.idx"));
    let tmp_path = dir.join(format!("{drive}.idx.tmp"));
    {
        let file = File::create(&tmp_path)?;
        bincode::serialize_into(BufWriter::new(file), &snapshot).map_err(io::Error::other)?;
    }
    fs::rename(&tmp_path, &final_path)
}
