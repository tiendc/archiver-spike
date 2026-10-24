//! How fast each format compresses and extracts, on a mix that looks like a
//! real folder: half real text in many files of all sizes (the Rust sources
//! in ~/.cargo/registry, which every machine running this has), photos
//! (incompressible) and a disk-image-like file (mostly zeros). Against the C
//! tools that happen to be installed, where there are any.
//!
//! Generated text is only a fallback: word salad from a small vocabulary is
//! the worst case for LZMA's match finder (7-Zip took 45 s for 64 MB of it)
//! and says nothing about real documents.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::Result;
use sevenz_rust2::encoder_options::Lzma2Options;
use sevenz_rust2::{ArchiveEntry, ArchiveReader, ArchiveWriter, Password, SourceReader};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

use crate::checks::which;
use crate::report::Report;

struct Row {
    format: &'static str,
    tool: String,
    size: u64,
    pack_s: Option<f64>,
    unpack_s: Option<f64>,
}

pub fn run(r: &mut Report) -> Result<()> {
    let mb: u64 = std::env::var("SPIKE_BENCH_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(192);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get() as u32);
    r.heading(&format!("Speed: {mb} MB mixed folder, {threads} threads"));

    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("folder");
    let total = make_folder(&src, mb << 20)?;
    let files = list(&src);
    let mut rows = Vec::new();

    let out = tmp.path().join("a.zip");
    let pack = time(|| zip_pack(&files, &src, &out))?;
    let unpack = time(|| zip_unpack(&out))?;
    rows.push(Row {
        format: "ZIP (Deflate 6)",
        tool: "zip crate (zlib-rs), 1 thread".into(),
        size: len(&out),
        pack_s: Some(pack),
        unpack_s: Some(unpack),
    });

    let rust_7z = tmp.path().join("a.7z");
    let out = rust_7z.clone();
    let pack = time(|| sevenz_pack(&files, &src, &out, threads))?;
    let unpack = time(|| sevenz_unpack(&out, threads))?;
    rows.push(Row {
        format: "7z (LZMA2 5, solid)",
        tool: format!("sevenz-rust2, {threads} threads"),
        size: len(&out),
        pack_s: Some(pack),
        unpack_s: Some(unpack),
    });

    let out = tmp.path().join("a.tar.zst");
    let pack = time(|| tar_zst_pack(&src, &out, threads))?;
    let unpack = time(|| tar_zst_unpack(&out))?;
    rows.push(Row {
        format: "tar.zst (level 3)",
        tool: format!("zstd crate (C libzstd), {threads} threads"),
        size: len(&out),
        pack_s: Some(pack),
        unpack_s: Some(unpack),
    });

    if which("zip") {
        let out = tmp.path().join("ref.zip");
        let pack = time(|| {
            run_tool(
                Command::new("zip")
                    .args(["-q", "-r", "-6"])
                    .arg(&out)
                    .arg(".")
                    .current_dir(&src),
            )
        })?;
        rows.push(Row {
            format: "ZIP (Deflate 6)",
            tool: "Info-ZIP `zip` (C), 1 thread".into(),
            size: len(&out),
            pack_s: Some(pack),
            unpack_s: None,
        });
    }
    for tool in ["7zz", "7z"] {
        if which(tool) {
            let out = tmp.path().join("ref.7z");
            let pack = time(|| {
                run_tool(
                    Command::new(tool)
                        .args(["a", "-bd", "-mx5", "-mmt=on"])
                        .arg(&out)
                        .arg(".")
                        .current_dir(&src),
                )
            })?;
            let unpack = time(|| {
                run_tool(
                    // `t` decodes everything without writing it, like the rows above.
                    Command::new(tool).args(["t", "-bd"]).arg(&out),
                )
            })?;
            rows.push(Row {
                format: "7z (LZMA2 5, solid)",
                tool: format!("7-Zip `{tool}` (C++), all threads"),
                size: len(&out),
                pack_s: Some(pack),
                unpack_s: Some(unpack),
            });
            // The same archive through both readers: a fair race, and proof
            // that each reads what the other wrote.
            let unpack = time(|| sevenz_unpack(&out, threads))?;
            rows.push(Row {
                format: "7z made by 7-Zip",
                tool: "read by sevenz-rust2".into(),
                size: len(&out),
                pack_s: None,
                unpack_s: Some(unpack),
            });
            let unpack = time(|| run_tool(Command::new(tool).args(["t", "-bd"]).arg(&rust_7z)))?;
            rows.push(Row {
                format: "7z made by sevenz-rust2",
                tool: format!("read by 7-Zip `{tool}`"),
                size: len(&rust_7z),
                pack_s: None,
                unpack_s: Some(unpack),
            });
            break;
        }
    }

    // RAR cannot be written by the product; rars writes one here only so the
    // two RAR readers have a large archive to race on.
    let rar = tmp.path().join("a.rar");
    let pack = time(|| rar_pack(&files, &src, &rar))?;
    rows.push(Row {
        format: "RAR5 (solid)",
        tool: "written by rars, as test data".into(),
        size: len(&rar),
        pack_s: Some(pack),
        unpack_s: None,
    });
    let helper = std::env::current_exe()?.with_file_name("rar-one");
    for (decoder, label) in [
        ("unrar", "read by UnRAR (C++)"),
        ("rars", "read by rars (Rust)"),
    ] {
        let unpack = time(|| rar_read(&helper, decoder, &rar))?;
        rows.push(Row {
            format: "RAR5 (solid)",
            tool: label.into(),
            size: len(&rar),
            pack_s: None,
            unpack_s: Some(unpack),
        });
    }

    let mbs = |s: f64| format!("{:.0} MB/s", total as f64 / 1e6 / s);
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                row.format.into(),
                row.tool.clone(),
                format!(
                    "{:.1} MB ({:.0}%)",
                    row.size as f64 / 1e6,
                    100.0 * row.size as f64 / total as f64
                ),
                row.pack_s
                    .map_or("–".into(), |s| format!("{s:.1} s · {}", mbs(s))),
                row.unpack_s
                    .map_or("–".into(), |s| format!("{s:.1} s · {}", mbs(s))),
            ]
        })
        .collect();
    r.table(
        &[
            "Format",
            "Tool",
            "Size",
            "Compress",
            "Extract (read all data)",
        ],
        &table,
    );
    if !which("7zz") && !which("7z") {
        r.line("7-Zip is not installed, so there is no 7-Zip row; `brew install sevenzip` and run again to compare.");
    }
    Ok(())
}

fn time(f: impl FnOnce() -> Result<()>) -> Result<f64> {
    let start = Instant::now();
    f()?;
    Ok(start.elapsed().as_secs_f64())
}

fn len(p: &Path) -> u64 {
    fs::metadata(p).map_or(0, |m| m.len())
}

fn run_tool(cmd: &mut Command) -> Result<()> {
    let out = cmd.output()?;
    anyhow::ensure!(
        out.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn list(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    files.sort();
    files
}

fn name(p: &Path, root: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Writes about `bytes` of mixed files; returns how much it wrote.
fn make_folder(dir: &Path, bytes: u64) -> Result<u64> {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let words: Vec<&str> =
        "the archive folder report contract invoice photo budget meeting notes project \
         design drawing schedule review final draft client payment summary quarterly"
            .split(' ')
            .collect();
    let real = copy_real_text(&dir.join("docs"), bytes / 2)?;
    let mut written = 0;
    let mut file = |path: PathBuf,
                    size: u64,
                    fill: &mut dyn FnMut(&mut BufWriter<File>, u64) -> io::Result<()>|
     -> Result<()> {
        fs::create_dir_all(path.parent().unwrap_or(dir))?;
        let mut w = BufWriter::new(File::create(&path)?);
        fill(&mut w, size)?;
        w.flush()?;
        written += size;
        Ok(())
    };
    let text_share = bytes * 40 / 100;
    for i in (0..24).filter(|_| real.is_none()) {
        file(
            dir.join(format!("docs/report-{i:02}.txt")),
            text_share / 24,
            &mut |w, size| {
                let mut n = 0;
                while n < size {
                    let word = words[rng.below(words.len())];
                    w.write_all(word.as_bytes())?;
                    w.write_all(if rng.below(12) == 0 { b"\n" } else { b" " })?;
                    n += word.len() as u64 + 1;
                }
                Ok(())
            },
        )?;
    }
    let photo_share = bytes * 35 / 100;
    for i in 0..8 {
        file(
            dir.join(format!("photos/IMG_{i:04}.jpg")),
            photo_share / 8,
            &mut |w, size| {
                let mut buf = vec![0u8; 1 << 16];
                let mut n = 0;
                while n < size {
                    rng.fill(&mut buf);
                    let take = buf.len().min((size - n) as usize);
                    w.write_all(&buf[..take])?;
                    n += take as u64;
                }
                Ok(())
            },
        )?;
    }
    file(dir.join("disk.img"), bytes * 15 / 100, &mut |w, size| {
        let mut buf = vec![0u8; 1 << 16];
        let mut n = 0;
        while n < size {
            buf.fill(0);
            if rng.below(8) == 0 {
                rng.fill(&mut buf[..4096]);
            }
            let take = buf.len().min((size - n) as usize);
            w.write_all(&buf[..take])?;
            n += take as u64;
        }
        Ok(())
    })?;
    let small_share = bytes * 10 / 100;
    let count = (small_share / 4096).max(1);
    for i in (0..count).filter(|_| real.is_none()) {
        file(
            dir.join(format!("small/{:02}/note-{i:05}.md", i % 50)),
            4096,
            &mut |w, size| {
                let line = format!(
                    "# Note {i}\n\n{}\n",
                    words[rng.below(words.len())].repeat(40)
                );
                let bytes = line.as_bytes();
                let mut n = 0;
                while n < size {
                    let take = bytes.len().min((size - n) as usize);
                    w.write_all(&bytes[..take])?;
                    n += take as u64;
                }
                Ok(())
            },
        )?;
    }
    Ok(written + real.unwrap_or(0))
}

/// Copies real text files (source, docs, config) into `dest` until about
/// `limit` bytes; `None` when there are not enough of them.
fn copy_real_text(dest: &Path, limit: u64) -> Result<Option<u64>> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let src = Path::new(&home).join(".cargo/registry/src");
    let mut copied = 0;
    for entry in WalkDir::new(&src)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if copied >= limit {
            break;
        }
        let path = entry.path();
        let is_text = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "rs" | "md" | "toml" | "txt" | "c" | "h" | "json"));
        let size = entry.metadata().map_or(0, |m| m.len());
        if !entry.file_type().is_file() || !is_text || size > 1 << 20 {
            continue;
        }
        let to = dest.join(path.strip_prefix(&src).unwrap_or(path));
        fs::create_dir_all(to.parent().unwrap_or(dest))?;
        fs::copy(path, &to)?;
        copied += size;
    }
    Ok((copied >= limit * 9 / 10).then_some(copied))
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

fn zip_pack(files: &[PathBuf], root: &Path, out: &Path) -> Result<()> {
    let mut zip = zip::ZipWriter::new(BufWriter::new(File::create(out)?));
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6))
        .large_file(true);
    for f in files {
        zip.start_file(name(f, root), opts)?;
        io::copy(&mut File::open(f)?, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}

fn zip_unpack(path: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    for i in 0..zip.len() {
        io::copy(&mut zip.by_index(i)?, &mut io::sink())?;
    }
    Ok(())
}

fn sevenz_pack(files: &[PathBuf], root: &Path, out: &Path, threads: u32) -> Result<()> {
    let mut w = ArchiveWriter::new(File::create(out)?)?;
    w.set_content_methods(vec![
        Lzma2Options::from_level_mt(5, threads, 4 << 20).into(),
    ]);
    let entries = files
        .iter()
        .map(|f| ArchiveEntry::from_path(f, name(f, root)))
        .collect();
    let readers = files
        .iter()
        .map(|f| File::open(f).map(SourceReader::new))
        .collect::<io::Result<Vec<_>>>()?;
    w.push_archive_entries(entries, readers)?;
    w.finish()?;
    Ok(())
}

fn sevenz_unpack(path: &Path, threads: u32) -> Result<()> {
    let mut reader = ArchiveReader::open(path, Password::empty())?;
    reader.set_thread_count(threads);
    reader.for_each_entries(|_, data| {
        io::copy(data, &mut io::sink())?;
        Ok(true)
    })?;
    Ok(())
}

fn tar_zst_pack(src: &Path, out: &Path, threads: u32) -> Result<()> {
    let mut enc = zstd::Encoder::new(BufWriter::new(File::create(out)?), 3)?;
    enc.multithread(threads)?;
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all(".", src)?;
    tar.into_inner()?.finish()?.flush()?;
    Ok(())
}

fn tar_zst_unpack(path: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(zstd::Decoder::new(File::open(path)?)?);
    for entry in tar.entries()? {
        io::copy(&mut entry?, &mut io::sink())?;
    }
    Ok(())
}

fn rar_pack(files: &[PathBuf], root: &Path, out: &Path) -> Result<()> {
    let mut b = rars::Builder::new(rars::ArchiveVersion::Rar50).solid(true);
    for f in files {
        b.add_path(f, name(f, root).as_bytes())?;
    }
    b.write_to_path(out, None)?;
    Ok(())
}

/// Reads a RAR through `rar-one`, so the time includes checksumming every
/// file, as an extraction would.
fn rar_read(helper: &Path, decoder: &str, archive: &Path) -> Result<()> {
    let out = Command::new(helper).arg(decoder).arg(archive).output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout.lines().last().unwrap_or_default();
    anyhow::ensure!(
        last.starts_with("ok"),
        "{decoder} could not read the RAR: {last}"
    );
    Ok(())
}
