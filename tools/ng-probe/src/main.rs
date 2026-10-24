//! unrar-ng (UnRAR 7.21) read the way rar-one and rar-perf read the others.
//!
//!   ng-probe read <archive>                  name<TAB>size<TAB>crc32 per file, then `ok` or `error`
//!   ng-probe time <archive> <how> [times]    how = test | read | extract-all
//!
//! `test` and `read` use the per-file API (`read_header` and `test` or
//! `read`), the one whose data can go through our own path checks;
//! `extract-all` is the batch API this fork adds, which lets UnRAR write the
//! files itself, past those checks.

use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use unrar_ng::Archive;

const PASSWORDS: [&str; 3] = ["", "password", "unrar"];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [cmd, archive] if cmd == "read" => {
            read(Path::new(archive));
            Ok(())
        }
        [cmd, archive, how, rest @ ..] if cmd == "time" => {
            let times = rest.first().and_then(|t| t.parse().ok()).unwrap_or(1);
            time(Path::new(archive), how, times)
        }
        _ => Err("usage: ng-probe read <archive> | time <archive> test|read|extract-all [times]".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

type Entries = Vec<(String, u64, u32)>;

fn entries(path: &Path, password: &str) -> Result<Entries, String> {
    let archive = if password.is_empty() {
        Archive::new(path)
    } else {
        Archive::with_password(path, password)
    };
    let mut open = archive.open_for_processing().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(header) = open.read_header().map_err(|e| e.to_string())? {
        if header.entry().is_file() {
            let name = header.entry().filename.to_string_lossy().replace('\\', "/");
            let (data, rest) = header.read().map_err(|e| e.to_string())?;
            out.push((name, data.len() as u64, crc32fast::hash(&data)));
            open = rest;
        } else {
            open = header.skip().map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}

fn read(path: &Path) {
    let mut last = String::new();
    for password in PASSWORDS {
        match entries(path, password) {
            Ok(mut files) => {
                files.sort();
                let mut out = io::stdout().lock();
                for (name, size, crc) in files {
                    let _ = writeln!(out, "{name}\t{size}\t{crc:08x}");
                }
                let _ = writeln!(out, "ok {password}");
                return;
            }
            Err(e) => last = e,
        }
    }
    println!("error {}", last.replace('\n', " "));
}

fn time(path: &Path, how: &str, times: usize) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..times {
        let start = Instant::now();
        let mut bytes = 0u64;
        match how {
            "test" | "read" => {
                let mut open = Archive::new(path).open_for_processing()?;
                while let Some(header) = open.read_header()? {
                    let size = header.entry().unpacked_size;
                    open = if !header.entry().is_file() {
                        header.skip()?
                    } else if how == "read" {
                        let (data, rest) = header.read()?;
                        std::hint::black_box(crc32fast::hash(&data));
                        bytes += data.len() as u64;
                        rest
                    } else {
                        bytes += size;
                        header.test()?
                    };
                }
            }
            "extract-all" => {
                let dest = std::env::temp_dir().join(format!("ng-probe-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dest);
                Archive::new(path).open_for_processing()?.extract_all(&dest)?;
                bytes = dir_size(&dest)?;
                std::fs::remove_dir_all(&dest)?;
            }
            other => return Err(format!("unknown way: {other}").into()),
        }
        let s = start.elapsed().as_secs_f64();
        println!("{how}: {:.0} MB in {s:.2} s = {:.0} MB/s", bytes as f64 / 1e6, bytes as f64 / 1e6 / s);
    }
    Ok(())
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() { dir_size(&entry.path())? } else { meta.len() };
    }
    Ok(total)
}
