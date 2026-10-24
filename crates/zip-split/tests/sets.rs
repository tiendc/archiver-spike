//! Split sets written by Info-ZIP 3.0 (`zip -s 64k`) and 7-Zip (`-v64k`),
//! each next to a reference: the set as 7-Zip extracts it, packed again as
//! one ordinary archive. Every file read through `SplitZip` must match it.
//! (Info-ZIP's own `zip -s 0` was not used for the references: the copy it
//! merged of the first set has a bad CRC in one entry, which the set itself
//! does not.)

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use spike_zip_split::{Error, SplitZip};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// Every file's contents by name (directories left out, as the references
/// were packed from a folder); the `zip` crate checks each CRC.
fn entries<R: Read + std::io::Seek>(reader: R) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(reader).expect("archive opens");
    let mut files = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("entry");
        if entry.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .expect("entry reads, CRC matches");
        files.insert(entry.name().expect("name").to_string(), data);
    }
    files
}

fn merged(name: &str) -> BTreeMap<String, Vec<u8>> {
    entries(File::open(corpus().join(format!("{name}-reference.zip"))).expect("reference"))
}

#[test]
fn info_zip_sets_read_like_their_merged_copies() {
    // The zip64 set (`zip -fz`, zip64 end records) holds the deflate set's files.
    for (name, reference) in [
        ("infozip-deflate", "infozip-deflate"),
        ("infozip-stored", "infozip-stored"),
        ("infozip-bigcd", "infozip-bigcd"),
        ("infozip-zip64", "infozip-deflate"),
    ] {
        let set = SplitZip::open(&corpus().join(format!("{name}.zip"))).expect("set opens");
        assert!(set.part_count() > 1, "{name}");
        let got = entries(set);
        let want = merged(reference);
        assert!(!want.is_empty(), "{name}");
        assert_eq!(got.len(), want.len(), "{name}");
        assert!(got == want, "{name}: entries differ from the merged copy");
    }
}

#[test]
fn a_central_directory_spanning_parts_is_read_whole() {
    // 1501 entries; the directory starts on the fourth part and ends on the
    // last one.
    let set = SplitZip::open(&corpus().join("infozip-bigcd.zip")).expect("set opens");
    assert_eq!(set.part_count(), 7);
    assert_eq!(zip::ZipArchive::new(set).expect("opens").len(), 1501);
}

#[test]
fn seven_zip_pieces_are_joined_without_rewriting() {
    let mut parts: Vec<_> = std::fs::read_dir(corpus())
        .expect("corpus")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.to_string_lossy().contains("sevenzip.zip."))
        .collect();
    parts.sort();
    assert!(parts.len() > 1);
    let total: u64 = parts
        .iter()
        .map(|p| p.metadata().expect("size").len())
        .sum();
    let files = parts.iter().map(|p| File::open(p).expect("part")).collect();
    let set = SplitZip::from_parts(files).expect("pieces open");
    // Nothing is rewritten: the archive is exactly the joined pieces.
    assert_eq!(set.len(), total);
    assert!(entries(set) == merged("sevenzip"));
}

#[test]
fn a_missing_part_is_named_before_reading() {
    let dir = tempfile::tempdir().expect("temp dir");
    for entry in std::fs::read_dir(corpus()).expect("corpus") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        if name.starts_with("infozip-deflate.") && name != "infozip-deflate.z02" {
            std::fs::copy(&path, dir.path().join(&name)).expect("copy");
        }
    }
    match SplitZip::open(&dir.path().join("infozip-deflate.zip")) {
        Err(Error::MissingPart { number: 2, path }) => {
            assert!(path.ends_with("infozip-deflate.z02"));
        }
        other => panic!("expected part 2 missing, got {other:?}"),
    }
}

#[test]
fn parts_given_out_of_count_are_refused() {
    let only_last = File::open(corpus().join("infozip-deflate.zip")).expect("last part");
    match SplitZip::from_parts(vec![only_last]) {
        Err(Error::PartCount {
            expected: 5,
            found: 1,
        }) => {}
        other => panic!("expected a part count error, got {other:?}"),
    }
}

#[test]
fn a_single_archive_passes_through() {
    let path = corpus().join("infozip-deflate-reference.zip");
    let len = path.metadata().expect("size").len();
    let set = SplitZip::from_parts(vec![File::open(&path).expect("archive")]).expect("opens");
    assert_eq!(set.len(), len);
    assert!(entries(set) == merged("infozip-deflate"));
}

#[test]
fn damaged_directories_are_errors_not_panics() {
    // Flip bytes across the last part, where the end records and most of the
    // directory are: opening and reading may fail, but must not panic.
    let dir = tempfile::tempdir().expect("temp dir");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(corpus()).expect("corpus") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        if name.starts_with("infozip-bigcd.z") {
            std::fs::copy(&path, dir.path().join(&name)).expect("copy");
            names.push(name);
        }
    }
    let last = dir.path().join("infozip-bigcd.zip");
    let original = std::fs::read(&last).expect("last part");
    let mut state = 0x2545_f491_u32;
    for _ in 0..300 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let mut damaged = original.clone();
        let at = state as usize % damaged.len();
        damaged[at] ^= (state >> 8) as u8 | 1;
        std::fs::write(&last, &damaged).expect("write");
        if let Ok(set) = SplitZip::open(&last)
            && let Ok(mut archive) = zip::ZipArchive::new(set)
        {
            for i in 0..archive.len().min(50) {
                if let Ok(mut entry) = archive.by_index(i) {
                    let _ = entry.read_to_end(&mut Vec::new());
                }
            }
        }
    }
}
