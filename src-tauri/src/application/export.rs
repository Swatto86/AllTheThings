//! Serialize a result set to an export file. Pure formatting over [`Hit`]s,
//! writing to any [`Write`] sink so it is testable without touching the disk.
//!
//! Three formats mirror voidtools Everything: plain text (one full path per
//! line), CSV (human-readable columns), and EFU (the Everything File List
//! format — full path, byte size, creation/modified times as Windows FILETIMEs,
//! and the numeric attribute mask).

use std::io::{self, Write};

use chrono::{Local, TimeZone};

use crate::application::index::Hit;

/// Milliseconds between the Windows (1601) and Unix (1970) epochs.
const FILETIME_UNIX_DIFF_MS: i64 = 11_644_473_600_000;

/// DOS attribute bits to display letters, in Explorer's order.
const ATTR_LETTERS: [(u32, char); 12] = [
    (0x1, 'R'),
    (0x2, 'H'),
    (0x4, 'S'),
    (0x20, 'A'),
    (0x10, 'D'),
    (0x400, 'L'),
    (0x200, 'P'),
    (0x800, 'C'),
    (0x4000, 'E'),
    (0x100, 'T'),
    (0x1000, 'O'),
    (0x2000, 'I'),
];

/// A supported export file format.
#[derive(Clone, Copy)]
pub enum ExportFormat {
    Csv,
    Txt,
    Efu,
}

impl ExportFormat {
    /// Parse the UI's format name, rejecting anything unknown.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "csv" => Some(Self::Csv),
            "txt" => Some(Self::Txt),
            "efu" => Some(Self::Efu),
            _ => None,
        }
    }
}

/// Write `hits` to `w` in the given format.
pub fn write_export(hits: &[Hit], format: ExportFormat, w: &mut impl Write) -> io::Result<()> {
    match format {
        ExportFormat::Txt => write_txt(hits, w),
        ExportFormat::Csv => write_csv(hits, w),
        ExportFormat::Efu => write_efu(hits, w),
    }
}

fn write_txt(hits: &[Hit], w: &mut impl Write) -> io::Result<()> {
    for h in hits {
        writeln!(w, "{}", h.path)?;
    }
    Ok(())
}

fn write_csv(hits: &[Hit], w: &mut impl Write) -> io::Result<()> {
    writeln!(
        w,
        "Name,Path,Size,Date Modified,Date Created,Date Accessed,Attributes"
    )?;
    for h in hits {
        writeln!(
            w,
            "{},{},{},{},{},{},{}",
            csv_field(&h.name),
            csv_field(&h.path),
            size_field(h),
            local_datetime(h.modified),
            local_datetime(h.created),
            local_datetime(h.accessed),
            attr_letters(h.attributes),
        )?;
    }
    Ok(())
}

fn write_efu(hits: &[Hit], w: &mut impl Write) -> io::Result<()> {
    writeln!(w, "Filename,Size,Date Modified,Date Created,Attributes")?;
    for h in hits {
        writeln!(
            w,
            "{},{},{},{},{}",
            csv_field(&h.path),
            efu_size(h),
            ms_to_filetime(h.modified),
            ms_to_filetime(h.created),
            h.attributes,
        )?;
    }
    Ok(())
}

/// Human-readable size in bytes, or empty for directories / unknown (CSV).
fn size_field(h: &Hit) -> String {
    if h.is_dir || h.size < 0 {
        String::new()
    } else {
        h.size.to_string()
    }
}

/// EFU's numeric Size column requires a number on every row; folders are `0`,
/// matching what voidtools Everything writes.
fn efu_size(h: &Hit) -> u64 {
    if h.is_dir || h.size < 0 {
        0
    } else {
        h.size as u64
    }
}

/// Quote a CSV field if it contains a comma, quote, or newline; double internal
/// quotes per RFC 4180.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Format Unix milliseconds as a local `YYYY-MM-DD HH:MM:SS`, or empty if unknown.
fn local_datetime(ms: i64) -> String {
    if ms == 0 {
        return String::new();
    }
    match Local.timestamp_millis_opt(ms).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => String::new(),
    }
}

/// Convert Unix milliseconds to a Windows FILETIME tick count (`0` if unknown).
fn ms_to_filetime(ms: i64) -> u64 {
    if ms == 0 {
        return 0;
    }
    ((ms + FILETIME_UNIX_DIFF_MS).max(0) as u64) * 10_000
}

fn attr_letters(attrs: u32) -> String {
    let mut s = String::new();
    for (bit, ch) in ATTR_LETTERS {
        if attrs & bit != 0 {
            s.push(ch);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, path: &str, is_dir: bool, size: i64, attrs: u32) -> Hit {
        Hit {
            name: name.into(),
            path: path.into(),
            size,
            modified: 0,
            created: 0,
            accessed: 0,
            attributes: attrs,
            is_dir,
        }
    }

    fn render(hits: &[Hit], format: ExportFormat) -> String {
        let mut buf = Vec::new();
        write_export(hits, format, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn txt_is_one_path_per_line() {
        let hits = vec![
            hit("a.txt", "C:\\a.txt", false, 1, 0x20),
            hit("b", "C:\\b", true, -1, 0x10),
        ];
        assert_eq!(render(&hits, ExportFormat::Txt), "C:\\a.txt\nC:\\b\n");
    }

    #[test]
    fn csv_quotes_commas_and_omits_dir_size() {
        let hits = vec![
            hit("a,b.txt", "C:\\x\\a,b.txt", false, 42, 0x2 | 0x20),
            hit("dir", "C:\\dir", true, -1, 0x10),
        ];
        let out = render(&hits, ExportFormat::Csv);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "Name,Path,Size,Date Modified,Date Created,Date Accessed,Attributes"
        );
        // Comma-containing fields are quoted; file size present, attrs as letters.
        assert!(lines[1].starts_with("\"a,b.txt\",\"C:\\x\\a,b.txt\",42,"));
        assert!(lines[1].ends_with(",HA"));
        // Directory: empty size, D attribute.
        assert!(lines[2].starts_with("dir,C:\\dir,,"));
        assert!(lines[2].ends_with(",D"));
    }

    #[test]
    fn efu_uses_filetime_and_numeric_attrs() {
        let mut h = hit("k.bin", "C:\\k.bin", false, 1000, 32);
        // 2024-06-15T12:00:00Z in Unix ms.
        h.modified = 1_718_452_800_000;
        let out = render(&[h], ExportFormat::Efu);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "Filename,Size,Date Modified,Date Created,Attributes"
        );
        // path, size, modified FILETIME, created (0, unknown), numeric attrs.
        assert_eq!(lines[1], "C:\\k.bin,1000,133629264000000000,0,32");
    }

    #[test]
    fn efu_directory_size_is_zero() {
        let hits = vec![hit("dir", "C:\\dir", true, -1, 0x10)];
        let out = render(&hits, ExportFormat::Efu);
        // Folder rows carry a numeric 0 size, not an empty field.
        assert_eq!(out.lines().nth(1).unwrap(), "C:\\dir,0,0,0,16");
    }

    #[test]
    fn attr_letters_order() {
        assert_eq!(attr_letters(0x2 | 0x20), "HA");
        assert_eq!(attr_letters(0x1 | 0x4 | 0x20), "RSA");
        assert_eq!(attr_letters(0), "");
    }
}
