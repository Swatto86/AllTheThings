//! File-content search ("content:"). Reads candidate files in the **caller's**
//! (the user's) token — never the LocalSystem service, whose pipe is query-only
//! — and keeps those whose body contains every content term. Bounded by a
//! per-file size cap and binary-file skipping, and cancellable mid-scan so a
//! superseded search stops promptly.

use std::fs::File;
use std::io::Read;

use rayon::prelude::*;

use crate::application::index::Hit;

/// Never read more than this from one file (a term past it isn't matched).
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// Bytes inspected for a NUL (binary marker) before decoding.
const SNIFF_BYTES: usize = 8192;
/// `FILE_ATTRIBUTE_REPARSE_POINT` — symlinks/junctions/mount points. Skipped so a
/// candidate that resolves to an offline network target or a device can't block a
/// reader thread forever (the read is byte-bounded, not time-bounded).
const REPARSE_POINT: u32 = 0x0000_0400;

/// Keep the candidate `hits` whose file body contains **all** `terms` (case
/// folded unless `match_case`). Directories, unreadable/binary files, and any
/// content past [`MAX_FILE_BYTES`] are skipped. `cancelled` is polled per file
/// so a superseded scan stops without reading the rest.
pub fn filter_by_content(
    hits: Vec<Hit>,
    terms: &[String],
    match_case: bool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Vec<Hit> {
    let needles: Vec<String> = if match_case {
        terms.to_vec()
    } else {
        terms.iter().map(|t| t.to_lowercase()).collect()
    };
    hits.into_par_iter()
        .filter(|h| {
            !h.is_dir
                && h.attributes & REPARSE_POINT == 0
                && !cancelled()
                && file_contains_all(&h.path, &needles, match_case)
        })
        .collect()
}

fn file_contains_all(path: &str, needles: &[String], match_case: bool) -> bool {
    let Some(text) = read_text(path) else {
        return false;
    };
    let hay = if match_case {
        text
    } else {
        text.to_lowercase()
    };
    needles.iter().all(|n| hay.contains(n.as_str()))
}

/// Read up to [`MAX_FILE_BYTES`] and decode to text, or `None` when the file
/// can't be read or looks binary. Never panics (all I/O errors → `None`).
fn read_text(path: &str) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_FILE_BYTES).read_to_end(&mut buf).ok()?;
    decode(&buf)
}

fn decode(bytes: &[u8]) -> Option<String> {
    // BOM-marked UTF-16 (Windows "Unicode" text) is decoded before the NUL
    // heuristic, which would otherwise read its high bytes as "binary".
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Some(decode_utf16(rest, true));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Some(decode_utf16(rest, false));
    }
    if bytes.iter().take(SNIFF_BYTES).any(|&b| b == 0) {
        return None; // binary
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_binary_and_text() {
        assert!(decode(b"hello world").is_some());
        assert!(decode(b"line1\nline2\r\n").is_some());
        // A NUL near the start marks binary.
        assert!(decode(b"PK\x03\x04\x00\x00stuff").is_none());
    }

    #[test]
    fn decodes_utf16le_bom() {
        // "Hi" as UTF-16 LE with BOM.
        let bytes = [0xFF, 0xFE, b'H', 0x00, b'i', 0x00];
        assert_eq!(decode(&bytes).as_deref(), Some("Hi"));
    }

    #[test]
    fn filter_keeps_matching_text_and_skips_binary() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("att-content-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
            p.to_string_lossy().into_owned()
        };
        let hit = |path: String| Hit {
            name: String::new(),
            path,
            size: 1,
            modified: 0,
            created: 0,
            accessed: 0,
            attributes: 0,
            is_dir: false,
        };
        let yes = write("a.txt", b"hello TIMEOUT world"); // case-insensitive match
        let no = write("b.txt", b"nothing relevant here");
        let bin = write("c.bin", b"\x00\x00timeout\x00"); // has the word but is binary
        let hits = vec![hit(yes.clone()), hit(no), hit(bin)];

        let never = || false;
        let out = filter_by_content(hits, &["timeout".to_string()], false, &never);
        let paths: Vec<&str> = out.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec![yes.as_str()]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
