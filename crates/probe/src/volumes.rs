//! A 7z split into parts (`name.7z.001`, `.002`…), read as the one file it
//! was cut from. 7-Zip splits at a byte count, with nothing in the parts
//! themselves, so joining them is all there is to it; sevenz-rust2 then
//! reads the joined stream like any 7z. What it does not do, and this does,
//! is say which part is missing before trying.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The parts found next to the first one, and what is missing.
#[derive(Debug)]
pub struct Volumes {
    pub parts: Vec<PathBuf>,
    /// Part numbers missing between the first and the last one found.
    pub gaps: Vec<u32>,
    /// The archive's full length, from its start header (first part only).
    pub expected_len: Option<u64>,
    pub found_len: u64,
}

impl Volumes {
    /// What to tell the user, if something is missing.
    pub fn problem(&self) -> Option<String> {
        if !self.gaps.is_empty() {
            let list: Vec<String> = self.gaps.iter().map(|n| format!("part {n}")).collect();
            return Some(format!("{} missing", list.join(", ")));
        }
        match self.expected_len {
            None => Some("part 1 missing (or not a 7z)".into()),
            Some(expected) if self.found_len < expected => Some(format!(
                "the last part(s) are missing: {} of {} bytes present",
                self.found_len, expected
            )),
            _ => None,
        }
    }
}

/// Looks for `<base>.001` … next to `any_part`.
pub fn scan(any_part: &Path) -> io::Result<Volumes> {
    let dir = any_part.parent().unwrap_or(Path::new("."));
    let name = any_part
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let base = name.rsplit_once('.').map_or(name, |(b, _)| b);
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(num) = file.strip_prefix(base).and_then(|r| r.strip_prefix('.'))
            && num.len() == 3
            && let Ok(n) = num.parse::<u32>()
        {
            found.push((n, path.clone()));
        }
    }
    found.sort();
    let last = found.last().map_or(0, |(n, _)| *n);
    let gaps = (1..=last)
        .filter(|n| !found.iter().any(|(f, _)| f == n))
        .collect();
    let mut found_len = 0;
    for (_, p) in &found {
        found_len += fs::metadata(p)?.len();
    }
    let expected_len = match found.first() {
        Some((1, p)) => start_header_len(p)?,
        _ => None,
    };
    Ok(Volumes {
        parts: found.into_iter().map(|(_, p)| p).collect(),
        gaps,
        expected_len,
        found_len,
    })
}

/// A 7z start header gives where the closing header is and how long it is,
/// so the first 32 bytes are enough to know the whole archive's length.
fn start_header_len(first: &Path) -> io::Result<Option<u64>> {
    let mut head = [0u8; 32];
    File::open(first)?.read_exact(&mut head)?;
    if head[..6] != *b"7z\xBC\xAF\x27\x1C" {
        return Ok(None);
    }
    let offset = u64::from_le_bytes(head[12..20].try_into().unwrap_or_default());
    let size = u64::from_le_bytes(head[20..28].try_into().unwrap_or_default());
    Ok(Some(32 + offset + size))
}

/// The parts, read as one.
pub struct Joined {
    parts: Vec<(File, u64)>,
    starts: Vec<u64>,
    len: u64,
    pos: u64,
}

impl Joined {
    pub fn open(volumes: &Volumes) -> io::Result<Self> {
        let mut parts = Vec::new();
        let mut starts = Vec::new();
        let mut len = 0;
        for p in &volumes.parts {
            let file = File::open(p)?;
            let size = file.metadata()?.len();
            starts.push(len);
            len += size;
            parts.push((file, size));
        }
        Ok(Self {
            parts,
            starts,
            len,
            pos: 0,
        })
    }
}

impl Read for Joined {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let i = self.starts.partition_point(|&s| s <= self.pos) - 1;
        let (file, size) = &mut self.parts[i];
        let within = self.pos - self.starts[i];
        file.seek(SeekFrom::Start(within))?;
        let want = buf.len().min((*size - within) as usize);
        let n = file.read(&mut buf[..want])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Joined {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos =
            pos.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}
