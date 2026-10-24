//! Where rars spends its time: one large RAR, read several ways.
//!
//!   rar-perf make <folder> <out.rar>      a solid RAR5 of the folder (slow: rars' writer)
//!   rar-perf make4 <folder> <out.rar>     the same as RAR 2.9 (RAR4)
//!   rar-perf make4lz <folder> <out.rar>   RAR4 with LZ only (no PPMd), like WinRAR's default
//!   rar-perf read <archive> <how> [times] how = sink | sink-handle | crc | parallel | unrar | unrar-test
//!                                         | unrar-pipe | rars-disk | rars-disk-pipelined | unrar-disk | unrar-pipe-disk
//!
//! The `-disk` ways extract for real, every file through spike-safe-path into
//! a new folder; `unrar-pipe` runs UnRAR in a child process (`rar-perf
//! child-unrar`) that sends each file over a pipe, as a sandboxed helper would.
//!
//! `sink` hands rars a writer that throws the bytes away, so only the
//! decoder is measured; `crc` adds what rar-one does (a lock and a CRC per
//! write); `unrar` and `unrar-test` are the C++ reference, with the data
//! copied out or only checked.

use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, folder, out] if cmd == "make" => make(
            Path::new(folder),
            Path::new(out),
            rars::ArchiveVersion::Rar50,
        ),
        [cmd, folder, out] if cmd == "make4" => make(
            Path::new(folder),
            Path::new(out),
            rars::ArchiveVersion::Rar29,
        ),
        // Level 2 skips rars' PPMd trial: LZ only, like WinRAR's default (m3).
        [cmd, folder, out] if cmd == "make4lz" => make_level(
            Path::new(folder),
            Path::new(out),
            rars::ArchiveVersion::Rar29,
            Some(2),
        ),
        [cmd, archive, how, rest @ ..] if cmd == "read" => {
            let times: usize = rest.first().and_then(|t| t.parse().ok()).unwrap_or(1);
            for _ in 0..times {
                let start = Instant::now();
                let bytes = read(Path::new(archive), how)?;
                let s = start.elapsed().as_secs_f64();
                println!(
                    "{how}: {:.0} MB in {s:.2} s = {:.0} MB/s",
                    bytes as f64 / 1e6,
                    bytes as f64 / 1e6 / s
                );
            }
            Ok(())
        }
        [cmd, archive] if cmd == "child-unrar" => child_unrar(Path::new(archive)),
        _ => Err("usage: rar-perf make <folder> <out.rar> | read <archive> <how> [times]".into()),
    }
}

fn make(
    folder: &Path,
    out: &Path,
    format: rars::ArchiveVersion,
) -> Result<(), Box<dyn std::error::Error>> {
    make_level(folder, out, format, None)
}

fn make_level(
    folder: &Path,
    out: &Path,
    format: rars::ArchiveVersion,
    level: Option<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut b = rars::Builder::new(format)
        .solid(true)
        .compression_level(level);
    let mut files: Vec<_> = walkdir::WalkDir::new(folder)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    files.sort();
    for f in &files {
        let name = f.strip_prefix(folder)?.to_string_lossy().into_owned();
        b.add_path(f, name.as_bytes())?;
    }
    let start = Instant::now();
    b.write_to_path(out, None)?;
    println!(
        "{} files written in {:.0} s",
        files.len(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Counts bytes, nothing else.
struct Count(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl Write for Count {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .fetch_add(buf.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What rar-one does per write: a lock and a CRC.
struct Crc(std::sync::Arc<std::sync::Mutex<(u64, crc32fast::Hasher)>>);

impl Write for Crc {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut g = self.0.lock().map_err(|_| io::Error::other("poisoned"))?;
        g.0 += buf.len() as u64;
        g.1.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read(archive: &Path, how: &str) -> Result<u64, Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    match how {
        "sink" | "parallel" | "sink-handle" => {
            // `read_path` opens the file again for every member; one handle
            // given to `read_reader` is kept for the whole extraction.
            let a = if how == "sink-handle" {
                rars::ArchiveReader::read_reader(std::fs::File::open(archive)?)?
            } else {
                rars::ArchiveReader::read_path(archive)?
            };
            let n = Arc::new(AtomicU64::new(0));
            let open = |_: &rars::ExtractedEntryMeta| -> rars::Result<Box<dyn Write>> {
                Ok(Box::new(Count(Arc::clone(&n))))
            };
            if how == "parallel" {
                a.extract_to_parallel_buffered(None, open)?;
            } else {
                a.extract_to(None, open)?;
            }
            Ok(n.load(Ordering::Relaxed))
        }
        "crc" => {
            let a = rars::ArchiveReader::read_path(archive)?;
            let state = Arc::new(Mutex::new((0, crc32fast::Hasher::new())));
            a.extract_to(None, |_| {
                Ok(Box::new(Crc(Arc::clone(&state))) as Box<dyn Write>)
            })?;
            let n = state.lock().map_err(|_| "poisoned")?.0;
            Ok(n)
        }
        "unrar" | "unrar-test" => {
            let mut open = unrar::Archive::new(archive).open_for_processing()?;
            let mut n = 0;
            while let Some(header) = open.read_header()? {
                let size = header.entry().unpacked_size;
                open = if !header.entry().is_file() {
                    header.skip()?
                } else if how == "unrar" {
                    let (data, rest) = header.read()?;
                    // The CRC is the work an extraction does; its value is not needed.
                    std::hint::black_box(crc32fast::hash(&data));
                    n += data.len() as u64;
                    rest
                } else {
                    n += size;
                    header.test()?
                };
            }
            Ok(n)
        }
        "unrar-pipe" | "unrar-pipe-disk" => {
            let to_disk = how == "unrar-pipe-disk";
            let tmp = tempfile::tempdir()?;
            let dest = spike_safe_path::Destination::new(tmp.path().join("out"))?;
            let mut child = std::process::Command::new(std::env::current_exe()?)
                .args(["child-unrar"])
                .arg(archive)
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let mut pipe =
                io::BufReader::with_capacity(1 << 20, child.stdout.take().ok_or("no pipe")?);
            let mut n = 0;
            let mut name_len = [0u8; 4];
            while io::Read::read_exact(&mut pipe, &mut name_len).is_ok() {
                let mut name = vec![0u8; u32::from_le_bytes(name_len) as usize];
                io::Read::read_exact(&mut pipe, &mut name)?;
                let mut len = [0u8; 8];
                io::Read::read_exact(&mut pipe, &mut len)?;
                let len = u64::from_le_bytes(len);
                let mut data = io::Read::take(&mut pipe, len);
                if to_disk {
                    write_file(&dest, &String::from_utf8_lossy(&name), &mut data)?;
                } else {
                    io::copy(&mut data, &mut io::sink())?;
                }
                n += len;
            }
            child.wait()?;
            Ok(n)
        }
        "unrar-disk" => {
            let tmp = tempfile::tempdir()?;
            let dest = spike_safe_path::Destination::new(tmp.path().join("out"))?;
            let mut open = unrar::Archive::new(archive).open_for_processing()?;
            let mut n = 0;
            while let Some(header) = open.read_header()? {
                open = if header.entry().is_file() {
                    let name = header.entry().filename.to_string_lossy().into_owned();
                    let (data, rest) = header.read()?;
                    write_file(&dest, &name, &mut data.as_slice())?;
                    n += data.len() as u64;
                    rest
                } else {
                    header.skip()?
                };
            }
            Ok(n)
        }
        "rars-disk" => {
            let tmp = tempfile::tempdir()?;
            let dest = spike_safe_path::Destination::new(tmp.path().join("out"))?;
            let a = rars::ArchiveReader::read_reader(std::fs::File::open(archive)?)?;
            let n = Arc::new(AtomicU64::new(0));
            a.extract_to(None, |meta| -> rars::Result<Box<dyn Write>> {
                if meta.is_directory {
                    return Ok(Box::new(io::sink()));
                }
                let name = String::from_utf8_lossy(&meta.name).into_owned();
                let file =
                    create(&dest, &name).map_err(|e| rars::Error::from(io::Error::other(e)))?;
                Ok(Box::new(Counted(
                    io::BufWriter::with_capacity(1 << 16, file),
                    Arc::clone(&n),
                )))
            })?;
            Ok(n.load(Ordering::Relaxed))
        }
        "rars-disk-pipelined" => {
            // Decoding on one thread, writing on another, as the helper process
            // and its parent do; the channel is bounded so memory stays small.
            let tmp = tempfile::tempdir()?;
            let dest = spike_safe_path::Destination::new(tmp.path().join("out"))?;
            let (tx, rx) = std::sync::mpsc::sync_channel::<Piece>(64);
            let path = archive.to_path_buf();
            let decoder = std::thread::spawn(move || -> Result<(), String> {
                let a = rars::ArchiveReader::read_reader(
                    std::fs::File::open(&path).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                a.extract_to(None, |meta| -> rars::Result<Box<dyn Write>> {
                    if meta.is_directory {
                        return Ok(Box::new(io::sink()));
                    }
                    let name = String::from_utf8_lossy(&meta.name).into_owned();
                    tx.send(Piece::File(name))
                        .map_err(|e| rars::Error::from(io::Error::other(e.to_string())))?;
                    Ok(Box::new(Sender(tx.clone())))
                })
                .map_err(|e| e.to_string())
            });
            let mut n = 0;
            let mut file: Option<io::BufWriter<std::fs::File>> = None;
            for piece in rx {
                match piece {
                    Piece::File(name) => {
                        file = Some(io::BufWriter::with_capacity(1 << 16, create(&dest, &name)?));
                    }
                    Piece::Data(bytes) => {
                        n += bytes.len() as u64;
                        file.as_mut()
                            .ok_or("data before a file")?
                            .write_all(&bytes)?;
                    }
                }
            }
            drop(file);
            decoder.join().map_err(|_| "decoder panicked")??;
            Ok(n)
        }
        other => Err(format!("unknown way to read: {other}").into()),
    }
}

/// The helper's side: every file, decoded by UnRAR, sent as
/// `name length (u32) | name | data length (u64) | data` on stdout.
fn child_unrar(archive: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = io::BufWriter::with_capacity(1 << 20, io::stdout().lock());
    let mut open = unrar::Archive::new(archive).open_for_processing()?;
    while let Some(header) = open.read_header()? {
        open = if header.entry().is_file() {
            let name = header.entry().filename.to_string_lossy().into_owned();
            let (data, rest) = header.read()?;
            out.write_all(&(name.len() as u32).to_le_bytes())?;
            out.write_all(name.as_bytes())?;
            out.write_all(&(data.len() as u64).to_le_bytes())?;
            out.write_all(&data)?;
            rest
        } else {
            header.skip()?
        };
    }
    out.flush()?;
    Ok(())
}

/// Creates a file the way the product would: the stored name through
/// `sanitize`, the file through `Destination`.
fn create(dest: &spike_safe_path::Destination, stored: &str) -> Result<std::fs::File, String> {
    let rel = spike_safe_path::sanitize(stored)
        .path()
        .cloned()
        .ok_or_else(|| format!("refused: {stored}"))?;
    dest.create_file(&rel).map_err(|e| e.to_string())
}

fn write_file(
    dest: &spike_safe_path::Destination,
    stored: &str,
    data: &mut impl io::Read,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = io::BufWriter::with_capacity(1 << 16, create(dest, stored)?);
    io::copy(data, &mut file)?;
    file.flush()?;
    Ok(())
}

/// A writer that counts what goes through it.
struct Counted<W>(W, std::sync::Arc<std::sync::atomic::AtomicU64>);

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.0.write(buf)?;
        self.1
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

enum Piece {
    File(String),
    Data(Vec<u8>),
}

/// Hands what rars writes to the writing thread.
struct Sender(std::sync::mpsc::SyncSender<Piece>);

impl Write for Sender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .send(Piece::Data(buf.to_vec()))
            .map_err(|_| io::Error::other("the writer stopped"))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
