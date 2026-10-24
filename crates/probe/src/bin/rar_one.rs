//! Reads one RAR archive with one decoder and prints what came out, one line
//! per file (`name<TAB>size<TAB>crc32`) and a last line `ok <password>` or
//! `error <message>`. The probe runs it as a child process, once per decoder
//! and archive, so a decoder that crashes or loops on a broken archive is
//! counted, not fatal: the same shape a sandboxed RAR helper would have.
//!
//!   rar-one unrar|rars <first part>
//!   rar-one repair <damaged.rar> <repaired.rar>   (rars, from the recovery record)
//!   rar-one make-rr <out.rar>                      (a RAR5 with a 10% recovery record)

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Tried in turn: libarchive's samples use "password", unrar.rs's "unrar".
const PASSWORDS: [&str; 3] = ["", "password", "unrar"];

type Entries = Vec<(String, u64, u32)>;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(decoder), Some(path)) = (args.next(), args.next()) else {
        eprintln!("usage: rar-one unrar|rars <archive>");
        std::process::exit(2);
    };
    let path = PathBuf::from(path);
    match decoder.as_str() {
        "repair" => return repair(&path, args.next()),
        "make-rr" => return make_with_recovery(&path),
        _ => {}
    }
    let mut last_error = String::new();
    for password in PASSWORDS {
        let result = match decoder.as_str() {
            "unrar" => with_unrar(&path, password),
            "rars" => with_rars(&path, password),
            other => Err(format!("unknown decoder {other}")),
        };
        match result {
            Ok(mut entries) => {
                entries.sort();
                let mut out = io::stdout().lock();
                for (name, size, crc) in entries {
                    let _ = writeln!(out, "{name}\t{size}\t{crc:08x}");
                }
                let _ = writeln!(out, "ok {password}");
                return;
            }
            Err(e) => last_error = e,
        }
    }
    println!("error {}", last_error.replace('\n', " "));
}

fn clean(name: &str) -> String {
    name.replace('\\', "/")
}

fn with_unrar(path: &Path, password: &str) -> Result<Entries, String> {
    let archive = if password.is_empty() {
        unrar::Archive::new(path)
    } else {
        unrar::Archive::with_password(path, password)
    };
    let mut open = archive.open_for_processing().map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    while let Some(header) = open.read_header().map_err(|e| e.to_string())? {
        let entry = header.entry();
        if entry.is_file() {
            let name = clean(&entry.filename.to_string_lossy());
            let (data, rest) = header.read().map_err(|e| e.to_string())?;
            entries.push((name, data.len() as u64, crc32fast::hash(&data)));
            open = rest;
        } else {
            open = header.skip().map_err(|e| e.to_string())?;
        }
    }
    Ok(entries)
}

/// Counts and checksums what rars writes for one entry.
struct Sink {
    entries: Arc<Mutex<Vec<(String, u64, crc32fast::Hasher)>>>,
    index: usize,
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        let entry = &mut entries[self.index];
        entry.1 += buf.len() as u64;
        entry.2.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn with_rars(path: &Path, password: &str) -> Result<Entries, String> {
    let parts = volume_parts(path);
    let pw = (!password.is_empty()).then_some(password.as_bytes());
    // Encrypted file lists need the password to be parsed at all.
    let options = || match pw {
        Some(p) => rars::ArchiveReadOptions::with_password(p),
        None => rars::ArchiveReadOptions::new(),
    };
    let archives = parts
        .iter()
        .map(|p| rars::ArchiveReader::read_path_with_options(p, options()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let shared = Arc::new(Mutex::new(Vec::new()));
    let open = |meta: &rars::ExtractedEntryMeta| -> rars::Result<Box<dyn Write>> {
        if meta.is_directory {
            return Ok(Box::new(io::sink()));
        }
        let mut entries = shared.lock().expect("not poisoned");
        entries.push((
            clean(&String::from_utf8_lossy(&meta.name)),
            0,
            crc32fast::Hasher::new(),
        ));
        Ok(Box::new(Sink {
            entries: Arc::clone(&shared),
            index: entries.len() - 1,
        }))
    };
    if archives.len() > 1 {
        rars::extract_volumes_to(&archives, pw, open).map_err(|e| e.to_string())?;
    } else {
        archives[0]
            .extract_to(pw, open)
            .map_err(|e| e.to_string())?;
    }
    let entries = std::mem::take(&mut *shared.lock().map_err(|_| "poisoned".to_string())?);
    Ok(entries
        .into_iter()
        .map(|(name, size, crc)| (name, size, crc.finalize()))
        .collect())
}

/// The parts of `name.partN.rar` next to the first one, in order; a single
/// archive is its own only part.
fn volume_parts(first: &Path) -> Vec<PathBuf> {
    let file = first
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let Some((stem, _)) = file.rsplit_once(".part") else {
        return vec![first.to_path_buf()];
    };
    let dir = first.parent().unwrap_or(Path::new("."));
    let mut parts: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_owned();
            let num = name
                .strip_prefix(stem)?
                .strip_prefix(".part")?
                .strip_suffix(".rar")?;
            Some((num.parse().ok()?, p))
        })
        .collect();
    parts.sort();
    parts.into_iter().map(|(_, p)| p).collect()
}

fn repair(damaged: &Path, out: Option<String>) {
    let Some(out) = out else {
        eprintln!("usage: rar-one repair <damaged> <out>");
        std::process::exit(2);
    };
    let result = rars::ArchiveReader::read_path(damaged)
        .and_then(|a| a.repair_recovery_with_report(None))
        .map_err(|e| e.to_string())
        .and_then(|r| {
            std::fs::write(&out, &r.data).map_err(|e| e.to_string())?;
            Ok(r.report)
        });
    match result {
        Ok(report) => println!(
            "ok repaired={} data_repaired={} shards={:?}/{:?}",
            report.changed,
            report.data_repaired,
            report.available_recovery_shards,
            report.expected_recovery_shards
        ),
        Err(e) => println!("error {e}"),
    }
}

/// Test data: 1 MiB that does not compress (like a photo), in a RAR5 with a
/// 10% recovery record, so a few KB of damage lands in the data.
fn make_with_recovery(out: &Path) {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let text: Vec<u8> = (0..1u32 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect();
    let mut b = rars::Builder::new(rars::ArchiveVersion::Rar50).recovery_percent(Some(10));
    let made = b
        .add_bytes(b"data.bin".to_vec(), text, None, None)
        .and_then(|_| b.write_to_path(out, None));
    match made {
        Ok(()) => println!("ok"),
        Err(e) => println!("error {e}"),
    }
}
