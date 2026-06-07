//! Live-volume engine tests. Ignored by default: they require Administrator
//! rights and a real NTFS system drive. Run explicitly with:
//!
//! ```text
//! cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture --test-threads=1
//! ```

use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use crate::application::{Catalog, SearchIndex, SearchOptions};
use crate::infrastructure::cache;
use crate::infrastructure::ntfs::{
    catch_up, journal_state, ntfs_fixed_drives, volume_serial, CatchUp, MftReader,
};

fn opts(query: &str) -> SearchOptions {
    SearchOptions {
        query: query.into(),
        limit: 20,
        ..SearchOptions::default()
    }
}

#[test]
#[ignore = "requires admin + live NTFS volume"]
fn index_all_volumes_and_search() {
    let drives = ntfs_fixed_drives();
    println!("fixed NTFS drives: {drives:?}");
    assert!(!drives.is_empty(), "no NTFS volumes discovered");

    let mut catalog = Catalog::default();
    let mut total_entries = 0usize;
    for drive in &drives {
        let mut reader = MftReader::open(*drive).expect("open volume");
        let progress = AtomicUsize::new(0);
        let started = Instant::now();
        let index = SearchIndex::build_from(&mut reader, &progress).expect("build index");
        total_entries += index.len();
        println!(
            "  {}: {} entries in {:?}",
            drive,
            index.len(),
            started.elapsed()
        );
        catalog.upsert_volume(index);
    }
    println!("catalog total: {total_entries} entries");

    // Hardlink expansion: System32\ntoskrnl.exe should now appear.
    let r = catalog.search(&opts("ntoskrnl"));
    println!("'ntoskrnl' -> {} total, {} ms", r.total, r.took_ms);
    for h in &r.hits {
        println!("  {}", h.path);
    }
    assert!(
        r.hits.iter().any(|h| h
            .path
            .to_lowercase()
            .contains(r"\windows\system32\ntoskrnl.exe")),
        "expected System32\\ntoskrnl.exe via hardlink expansion"
    );

    // Wildcard pattern.
    let r = catalog.search(&opts("ntoskrnl.*"));
    println!("wildcard 'ntoskrnl.*' -> {} total", r.total);
    assert!(r.total > 0, "wildcard search returned nothing");

    // ext: operator.
    let r = catalog.search(&opts("ext:dll"));
    println!("'ext:dll' -> {} total in {} ms", r.total, r.took_ms);
    assert!(r.total > 100, "expected many DLLs");
    assert!(
        r.hits
            .iter()
            .all(|h| h.name.to_lowercase().ends_with(".dll")),
        "ext:dll returned a non-dll"
    );

    // folder: operator returns only directories.
    let r = catalog.search(&opts("folder: windows"));
    println!("'folder: windows' -> {} total", r.total);
    assert!(r.hits.iter().all(|h| h.is_dir), "folder: returned a file");

    // size: operator returns only files above the threshold.
    let r = catalog.search(&opts("size:>100mb"));
    println!("'size:>100mb' -> {} total", r.total);
    assert!(
        r.hits
            .iter()
            .all(|h| !h.is_dir && h.size > 100 * 1024 * 1024),
        "size:>100mb returned a folder or small file"
    );

    // attrib: filter — the directory bit is normalized from the record header.
    let r = catalog.search(&opts("attrib:d"));
    println!("'attrib:d' -> {} total", r.total);
    assert!(r.hits.iter().all(|h| h.is_dir), "attrib:d returned a file");

    // Creation/access times are captured by the full scan.
    let r = catalog.search(&opts("ext:dll"));
    assert!(
        r.hits.iter().any(|h| h.created > 0),
        "no creation times captured"
    );
    assert!(
        r.hits.iter().any(|h| h.accessed > 0),
        "no access times captured"
    );

    // dm: date filter compiles and runs against the live index.
    let r = catalog.search(&opts("dm:>=2000-01-01"));
    println!("'dm:>=2000-01-01' -> {} total", r.total);
    assert!(r.total > 0, "date filter returned nothing");

    // Full cache round-trip through disk + USN catch-up.
    let drive0 = drives[0];
    let serial = volume_serial(drive0);
    let journal = journal_state(drive0).expect("journal state");
    let exported = catalog.volume(drive0).expect("volume present").export();
    println!("exported {} entries for {}:", exported.len(), drive0);

    cache::save(
        drive0,
        serial,
        journal.journal_id,
        journal.next_usn,
        exported,
    )
    .expect("cache save");
    let snapshot = cache::load(drive0).expect("cache load");
    assert_eq!(snapshot.journal_id, journal.journal_id);
    assert_eq!(snapshot.volume_serial, serial);

    let from_usn = snapshot.next_usn;
    let mut reloaded_index = SearchIndex::import(drive0, snapshot.entries);
    match catch_up(drive0, snapshot.journal_id, from_usn, &mut reloaded_index) {
        CatchUp::Caught { next_usn } => {
            println!("catch_up: Caught (replayed to usn {next_usn})");
            assert!(next_usn >= from_usn);
        }
        CatchUp::Stale => panic!("catch_up unexpectedly Stale"),
    }

    let mut reloaded = Catalog::default();
    reloaded.upsert_volume(reloaded_index);
    let r = reloaded.search(&opts("ntoskrnl"));
    assert!(
        r.hits.iter().any(|h| h
            .path
            .to_lowercase()
            .contains(r"\windows\system32\ntoskrnl.exe")),
        "cache round-trip lost System32\\ntoskrnl.exe"
    );
    println!("cache round-trip OK: 'ntoskrnl' -> {} hits", r.total);

    // Empty query (default name-sorted view) is instant.
    let r = catalog.search(&opts(""));
    println!("empty query -> {} total in {} ms", r.total, r.took_ms);
    assert_eq!(r.total, total_entries);
}
