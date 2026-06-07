//! Core search-index value types. No I/O, no platform code.

/// An NTFS MFT file-record number. Newtype so raw `u64`s can't be confused
/// with sizes, offsets, or other identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordId(pub u64);

/// A search-ready file or directory entry held in the in-memory index.
///
/// `name_lower` is precomputed so per-keystroke search never re-lowercases.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub record: RecordId,
    pub parent: RecordId,
    pub name: String,
    pub name_lower: String,
    pub is_dir: bool,
    /// File size in bytes; `None` for directories or unknown.
    pub size: Option<u64>,
    /// Last-modified time in Unix milliseconds; `None` if unknown.
    pub modified_ms: Option<i64>,
}
