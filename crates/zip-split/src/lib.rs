//! Split ZIP sets (`name.z01`, `name.z02`, …, `name.zip`), as Info-ZIP's
//! `zip -s` and WinZip write them, read with the `zip` crate, which reads
//! single-disk archives only.
//!
//! The parts ("disks") are consecutive slices of one stream, but the central
//! directory gives every entry's offset from the start of the disk it begins
//! on, and its own offset from the start of the disk it begins on. So the
//! parts are joined, and the central directory is rewritten as for one disk:
//! every offset into the joined stream, every disk number 0. The `zip` crate
//! then reads [`SplitZip`] like any archive, and nothing is copied to disk.
//!
//! 7-Zip's `name.zip.001`, `.002`… are not that: 7-Zip cuts a finished
//! single-disk archive at a byte count. [`SplitZip::from_parts`] takes those
//! too; their directory needs no rewriting, so it is passed through as is.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const EOCD: u32 = 0x0605_4b50;
const ZIP64_EOCD: u32 = 0x0606_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const ZIP64_EXTRA: u16 = 0x0001;
const EOCD_LEN: usize = 22;
const LOCATOR_LEN: usize = 20;
const ZIP64_EOCD_LEN: usize = 56;
const CENTRAL_LEN: usize = 46;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("part {number} of the set is missing ({path})")]
    MissingPart { number: u32, path: PathBuf },
    #[error("not a ZIP archive: no end of central directory record in the last part")]
    NoEnd,
    #[error("the archive says it has {expected} parts, but {found} were given")]
    PartCount { expected: u64, found: usize },
    #[error("invalid split archive: {0}")]
    Invalid(&'static str),
}

/// A split set read as one single-disk archive.
#[derive(Debug)]
pub struct SplitZip {
    parts: Vec<File>,
    /// Where each part starts in the joined stream; one more entry, the end.
    starts: Vec<u64>,
    /// How much of the joined stream is kept: everything before the original
    /// central directory, or all of it when nothing is rewritten.
    data_len: u64,
    /// The rewritten central directory and end records, after `data_len`.
    tail: Vec<u8>,
    pos: u64,
}

impl SplitZip {
    /// Opens the set whose last part is `last` (`name.zip`), finding
    /// `name.z01` … next to it. The last part says how many there are.
    pub fn open(last: &Path) -> Result<Self, Error> {
        let mut file = File::open(last)?;
        let len = file.seek(SeekFrom::End(0))?;
        let (end, _) = find_end(&mut file, len)?;
        // The zip64 locator, when there is one, has the true count.
        let disks = match read_locator(&mut file, end.position)? {
            Some(locator) => u64::from(locator.total_disks),
            None => u64::from(end.disk_number) + 1,
        };
        let mut paths = Vec::new();
        for number in 1..disks {
            let number = u32::try_from(number).map_err(|_| Error::Invalid("too many parts"))?;
            let path = sibling(last, number);
            if !path.is_file() {
                return Err(Error::MissingPart { number, path });
            }
            paths.push(path);
        }
        let mut parts = paths
            .iter()
            .map(File::open)
            .collect::<io::Result<Vec<_>>>()?;
        parts.push(file);
        Self::from_parts(parts)
    }

    /// The parts in order: a split set's disks, or the pieces of an archive
    /// cut at a byte count (`.zip.001` …). A single file works as well.
    pub fn from_parts(mut parts: Vec<File>) -> Result<Self, Error> {
        if parts.is_empty() {
            return Err(Error::Invalid("no parts"));
        }
        let mut starts = Vec::with_capacity(parts.len() + 1);
        let mut total = 0u64;
        for part in &mut parts {
            starts.push(total);
            let len = part.seek(SeekFrom::End(0))?;
            total = total
                .checked_add(len)
                .ok_or(Error::Invalid("parts too large"))?;
        }
        starts.push(total);
        let mut joined = Self {
            parts,
            starts,
            data_len: total,
            tail: Vec::new(),
            pos: 0,
        };
        joined.rewrite()?;
        joined.pos = 0;
        Ok(joined)
    }

    /// The length of the archive as the `zip` crate sees it.
    pub fn len(&self) -> u64 {
        self.data_len + self.tail.len() as u64
    }

    /// Whether there is nothing in it (never, for an archive that opened).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many parts the set has.
    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    fn rewrite(&mut self) -> Result<(), Error> {
        let last = self.parts.len() - 1;
        let last_start = self.starts[last];
        let last_len = self.starts[last + 1] - last_start;
        let (end, end_bytes) = find_end(&mut self.parts[last], last_len)?;
        let end_global = last_start + end.position;
        let mut info = Directory {
            disks: u64::from(end.disk_number) + 1,
            cd_disk: u64::from(end.cd_disk),
            entries: u64::from(end.entries),
            cd_size: u64::from(end.cd_size),
            cd_offset: u64::from(end.cd_offset),
            version_made_by: 45,
        };
        let mut records_start = end_global;
        if let Some(locator) = read_locator(&mut self.parts[last], end.position)? {
            records_start = end_global - LOCATOR_LEN as u64;
            info.disks = u64::from(locator.total_disks);
            let at = self.disk_start(u64::from(locator.disk), locator.offset)?;
            let mut record = [0u8; ZIP64_EOCD_LEN];
            self.read_at(at, &mut record)?;
            if u32_at(&record, 0) != ZIP64_EOCD {
                return Err(Error::Invalid(
                    "zip64 end of central directory record not found",
                ));
            }
            info.version_made_by = u16_at(&record, 12);
            info.cd_disk = u64::from(u32_at(&record, 20));
            info.entries = u64_at(&record, 32);
            info.cd_size = u64_at(&record, 40);
            info.cd_offset = u64_at(&record, 48);
            records_start = records_start.min(at);
        }
        if info.disks == 1 {
            // One disk, however many pieces it was cut into: nothing to do.
            return Ok(());
        }
        if info.disks != self.parts.len() as u64 {
            return Err(Error::PartCount {
                expected: info.disks,
                found: self.parts.len(),
            });
        }
        let cd_start = self.disk_start(info.cd_disk, info.cd_offset)?;
        let cd_end = cd_start
            .checked_add(info.cd_size)
            .filter(|&end| end <= records_start)
            .ok_or(Error::Invalid(
                "central directory runs past the end records",
            ))?;
        let cd_len = usize::try_from(cd_end - cd_start)
            .map_err(|_| Error::Invalid("central directory too large"))?;
        let mut cd = vec![0u8; cd_len];
        self.read_at(cd_start, &mut cd)?;

        let mut out = Vec::with_capacity(cd.len() + 128);
        let mut at = 0usize;
        for _ in 0..info.entries {
            at = self.rewrite_entry(&cd, at, &mut out)?;
        }
        if at != cd.len() {
            return Err(Error::Invalid(
                "central directory size does not match its entries",
            ));
        }

        // The joined stream up to the old directory, then the new one.
        self.data_len = cd_start;
        let comment = &end_bytes[EOCD_LEN..];
        write_end(&mut out, &info, cd_start, comment)?;
        self.tail = out;
        Ok(())
    }

    /// Copies the central header at `cd[at..]` into `out` for a single disk,
    /// and returns where the next one starts.
    fn rewrite_entry(&self, cd: &[u8], at: usize, out: &mut Vec<u8>) -> Result<usize, Error> {
        let fixed = cd
            .get(at..at + CENTRAL_LEN)
            .ok_or(Error::Invalid("central directory ends inside a header"))?;
        if u32_at(fixed, 0) != CENTRAL {
            return Err(Error::Invalid("central header signature missing"));
        }
        let compressed = u32_at(fixed, 20);
        let uncompressed = u32_at(fixed, 24);
        let name_len = usize::from(u16_at(fixed, 28));
        let extra_len = usize::from(u16_at(fixed, 30));
        let comment_len = usize::from(u16_at(fixed, 32));
        let disk = u16_at(fixed, 34);
        let offset = u32_at(fixed, 42);
        let name_end = at + CENTRAL_LEN + name_len;
        let extra_end = name_end + extra_len;
        let next = extra_end + comment_len;
        if next > cd.len() {
            return Err(Error::Invalid("central header runs past the directory"));
        }

        // The zip64 extra holds, in order, whichever of these the fixed
        // fields mark as too large: sizes, local header offset, disk.
        let mut big_uncompressed = None;
        let mut big_compressed = None;
        let mut big_offset = None;
        let mut big_disk = None;
        let mut other_extras = Vec::new();
        let mut extra = &cd[name_end..extra_end];
        while !extra.is_empty() {
            if extra.len() < 4 {
                return Err(Error::Invalid("truncated extra field"));
            }
            let id = u16_at(extra, 0);
            let size = usize::from(u16_at(extra, 2));
            let body = extra
                .get(4..4 + size)
                .ok_or(Error::Invalid("extra field runs past its header"))?;
            if id == ZIP64_EXTRA {
                let mut values = body;
                let mut take8 = |marked: bool| -> Result<Option<u64>, Error> {
                    if !marked {
                        return Ok(None);
                    }
                    let value = values.get(..8).ok_or(Error::Invalid("short zip64 extra"))?;
                    values = &values[8..];
                    Ok(Some(u64_at(value, 0)))
                };
                big_uncompressed = take8(uncompressed == u32::MAX)?;
                big_compressed = take8(compressed == u32::MAX)?;
                big_offset = take8(offset == u32::MAX)?;
                if disk == u16::MAX {
                    let value = values.get(..4).ok_or(Error::Invalid("short zip64 extra"))?;
                    big_disk = Some(u32_at(value, 0));
                }
            } else {
                other_extras.extend_from_slice(&extra[..4 + size]);
            }
            extra = &extra[4 + size..];
        }

        let disk = big_disk.map_or(u64::from(disk), u64::from);
        let offset = big_offset.unwrap_or(u64::from(offset));
        let global = self.disk_start(disk, offset)?;

        let mut zip64 = Vec::new();
        for value in [big_uncompressed, big_compressed].into_iter().flatten() {
            zip64.extend_from_slice(&value.to_le_bytes());
        }
        let fixed_offset = if global >= u64::from(u32::MAX) {
            zip64.extend_from_slice(&global.to_le_bytes());
            u32::MAX
        } else {
            global as u32
        };
        let mut extras = Vec::with_capacity(other_extras.len() + zip64.len() + 4);
        if !zip64.is_empty() {
            extras.extend_from_slice(&ZIP64_EXTRA.to_le_bytes());
            extras.extend_from_slice(&(zip64.len() as u16).to_le_bytes());
            extras.extend_from_slice(&zip64);
        }
        extras.extend_from_slice(&other_extras);
        let extras_len =
            u16::try_from(extras.len()).map_err(|_| Error::Invalid("extra fields too large"))?;

        let start = out.len();
        out.extend_from_slice(fixed);
        out[start + 30..start + 32].copy_from_slice(&extras_len.to_le_bytes());
        out[start + 34..start + 36].copy_from_slice(&0u16.to_le_bytes());
        out[start + 42..start + 46].copy_from_slice(&fixed_offset.to_le_bytes());
        out.extend_from_slice(&cd[at + CENTRAL_LEN..name_end]);
        out.extend_from_slice(&extras);
        out.extend_from_slice(&cd[extra_end..next]);
        Ok(next)
    }

    /// Where `offset` on disk `disk` is in the joined stream.
    fn disk_start(&self, disk: u64, offset: u64) -> Result<u64, Error> {
        let disk = usize::try_from(disk)
            .ok()
            .filter(|&d| d < self.parts.len())
            .ok_or(Error::Invalid("disk number past the last part"))?;
        self.starts[disk]
            .checked_add(offset)
            .filter(|&at| at <= self.starts[self.parts.len()])
            .ok_or(Error::Invalid("offset past the end of the set"))
    }

    /// Reads exactly `buf.len()` bytes of the joined stream from `at`.
    fn read_at(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < buf.len() {
            let n = self.read_joined(at + done as u64, &mut buf[done..])?;
            if n == 0 {
                return Err(Error::Invalid("read past the end of the set"));
            }
            done += n;
        }
        Ok(())
    }

    /// Reads from the joined parts (not the rewritten tail), within one part.
    fn read_joined(&mut self, at: u64, buf: &mut [u8]) -> io::Result<usize> {
        let end = self.starts[self.parts.len()];
        if at >= end {
            return Ok(0);
        }
        // The last part whose start is at or before `at`.
        let part = self.starts.partition_point(|&start| start <= at) - 1;
        let within = at - self.starts[part];
        let room = (self.starts[part + 1] - at).min(buf.len() as u64) as usize;
        let file = &mut self.parts[part];
        file.seek(SeekFrom::Start(within))?;
        file.read(&mut buf[..room])
    }
}

impl Read for SplitZip {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = if self.pos < self.data_len {
            let room = (self.data_len - self.pos).min(buf.len() as u64) as usize;
            self.read_joined(self.pos, &mut buf[..room])?
        } else {
            let at = (self.pos - self.data_len) as usize;
            let rest = self.tail.get(at..).unwrap_or_default();
            let n = rest.len().min(buf.len());
            buf[..n].copy_from_slice(&rest[..n]);
            n
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SplitZip {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(delta) => self.len().checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        let target = target
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        self.pos = target;
        Ok(target)
    }
}

/// `name.z01` … for part `number` of the set ending in `name.zip`, in the
/// same letter case as the last part's extension.
pub fn sibling(last: &Path, number: u32) -> PathBuf {
    let upper = last
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.chars().all(|c| c.is_ascii_uppercase()));
    let ext = format!("{}{number:02}", if upper { 'Z' } else { 'z' });
    last.with_extension(ext)
}

#[derive(Debug)]
struct End {
    /// Where the record starts in its part.
    position: u64,
    disk_number: u16,
    cd_disk: u16,
    entries: u16,
    cd_size: u32,
    cd_offset: u32,
}

#[derive(Debug)]
struct Locator {
    disk: u32,
    offset: u64,
    total_disks: u32,
}

struct Directory {
    disks: u64,
    cd_disk: u64,
    entries: u64,
    cd_size: u64,
    cd_offset: u64,
    version_made_by: u16,
}

/// The end of central directory record in the last `len` bytes of `file`,
/// and its bytes (comment included).
fn find_end(file: &mut File, len: u64) -> Result<(End, Vec<u8>), Error> {
    let window = len.min((EOCD_LEN + usize::from(u16::MAX)) as u64);
    let mut tail = vec![0u8; window as usize];
    file.seek(SeekFrom::Start(len - window))?;
    file.read_exact(&mut tail)?;
    // The last signature whose comment ends at or before the end of the file.
    let found = (0..tail.len().saturating_sub(EOCD_LEN - 1))
        .rev()
        .find(|&i| {
            u32_at(&tail, i) == EOCD
                && i + EOCD_LEN + usize::from(u16_at(&tail, i + 20)) <= tail.len()
        });
    let i = found.ok_or(Error::NoEnd)?;
    let comment_len = usize::from(u16_at(&tail, i + 20));
    let record = tail[i..i + EOCD_LEN + comment_len].to_vec();
    Ok((
        End {
            position: len - window + i as u64,
            disk_number: u16_at(&record, 4),
            cd_disk: u16_at(&record, 6),
            entries: u16_at(&record, 10),
            cd_size: u32_at(&record, 12),
            cd_offset: u32_at(&record, 16),
        },
        record,
    ))
}

/// The zip64 end of central directory locator just before the end record at
/// `end_position`, if there is one.
fn read_locator(file: &mut File, end_position: u64) -> Result<Option<Locator>, Error> {
    let Some(at) = end_position.checked_sub(LOCATOR_LEN as u64) else {
        return Ok(None);
    };
    let mut record = [0u8; LOCATOR_LEN];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut record)?;
    if u32_at(&record, 0) != ZIP64_LOCATOR {
        return Ok(None);
    }
    Ok(Some(Locator {
        disk: u32_at(&record, 4),
        offset: u64_at(&record, 8),
        total_disks: u32_at(&record, 16),
    }))
}

/// The end records for one disk after the directory already in `out`:
/// zip64 ones first when a value needs them, then the end record carrying
/// the archive comment.
fn write_end(
    out: &mut Vec<u8>,
    info: &Directory,
    cd_offset: u64,
    comment: &[u8],
) -> Result<(), Error> {
    let cd_size = out.len() as u64;
    let needs_zip64 = info.entries >= u64::from(u16::MAX)
        || cd_size >= u64::from(u32::MAX)
        || cd_offset >= u64::from(u32::MAX);
    if needs_zip64 {
        let zip64_at = cd_offset + cd_size;
        out.extend_from_slice(&ZIP64_EOCD.to_le_bytes());
        out.extend_from_slice(&((ZIP64_EOCD_LEN - 12) as u64).to_le_bytes());
        out.extend_from_slice(&info.version_made_by.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&info.entries.to_le_bytes());
        out.extend_from_slice(&info.entries.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&ZIP64_LOCATOR.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&zip64_at.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    let entries = u16::try_from(info.entries).unwrap_or(u16::MAX);
    let size = u32::try_from(cd_size).unwrap_or(u32::MAX);
    let offset = u32::try_from(cd_offset).unwrap_or(u32::MAX);
    out.extend_from_slice(&EOCD.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&entries.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    let comment_len =
        u16::try_from(comment.len()).map_err(|_| Error::Invalid("archive comment too long"))?;
    out.extend_from_slice(&comment_len.to_le_bytes());
    out.extend_from_slice(comment);
    Ok(())
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}
