//! ZIP and TAR written byte by byte, so a test archive can say exactly what
//! a hostile or careless one says: names with `..` or no UTF-8 flag, sizes
//! that lie, links that point out. Real writers refuse to produce these.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// 2026-10-23 12:00, in DOS form.
const DOS_TIME: u16 = 12 << 11;
const DOS_DATE: u16 = ((2026 - 1980) << 9) | (10 << 5) | 23;
const FLAG_UTF8: u16 = 1 << 11;
const UNIX_HOST: u16 = 3 << 8;
const S_IFLNK: u32 = 0o120_000;
const S_IFREG: u32 = 0o100_000;

/// One entry's data, ready to store.
pub struct Body {
    method: u16,
    bytes: Vec<u8>,
    crc: u32,
    size: u64,
}

impl Body {
    pub fn stored(data: &[u8]) -> Self {
        Self {
            method: 0,
            bytes: data.to_vec(),
            crc: crc32fast::hash(data),
            size: data.len() as u64,
        }
    }

    /// `size` zero bytes, deflated as they are produced (a bomb fits in memory).
    pub fn deflated_zeros(size: u64) -> std::io::Result<Self> {
        let chunk = vec![0u8; 1 << 20];
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        let mut crc = crc32fast::Hasher::new();
        let mut left = size;
        while left > 0 {
            let n = left.min(chunk.len() as u64) as usize;
            enc.write_all(&chunk[..n])?;
            crc.update(&chunk[..n]);
            left -= n as u64;
        }
        Ok(Self {
            method: 8,
            bytes: enc.finish()?,
            crc: crc.finalize(),
            size,
        })
    }
}

pub struct Zip {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
    /// Each entry's central record without its name, for [`Zip::alias`].
    records: Vec<Vec<u8>>,
}

/// How an entry is described, apart from its name and data.
#[derive(Clone, Copy, Default)]
pub struct Meta {
    pub utf8_flag: bool,
    /// Unix mode bits; a symlink is `Meta::symlink()`.
    pub unix_mode: Option<u32>,
    /// A size to claim instead of the real one.
    pub claimed_size: Option<u32>,
}

impl Meta {
    pub fn utf8() -> Self {
        Self {
            utf8_flag: true,
            ..Self::default()
        }
    }

    pub fn symlink() -> Self {
        Self {
            utf8_flag: true,
            unix_mode: Some(S_IFLNK | 0o777),
            ..Self::default()
        }
    }

    pub fn file() -> Self {
        Self {
            utf8_flag: true,
            unix_mode: Some(S_IFREG | 0o644),
            ..Self::default()
        }
    }
}

impl Zip {
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            central: Vec::new(),
            count: 0,
            records: Vec::new(),
        }
    }

    pub fn add(&mut self, name: &[u8], body: &Body, meta: Meta) -> &mut Self {
        let offset = self.out.len() as u32;
        let flags = if meta.utf8_flag { FLAG_UTF8 } else { 0 };
        let size = meta.claimed_size.unwrap_or(body.size as u32);
        let name_len = name.len() as u16;
        let csize = body.bytes.len() as u32;

        let o = &mut self.out;
        o.extend(0x0403_4b50u32.to_le_bytes());
        o.extend(20u16.to_le_bytes());
        o.extend(flags.to_le_bytes());
        o.extend(body.method.to_le_bytes());
        o.extend(DOS_TIME.to_le_bytes());
        o.extend(DOS_DATE.to_le_bytes());
        o.extend(body.crc.to_le_bytes());
        o.extend(csize.to_le_bytes());
        o.extend(size.to_le_bytes());
        o.extend(name_len.to_le_bytes());
        o.extend(0u16.to_le_bytes());
        o.extend(name);
        o.extend(&body.bytes);

        let (made_by, external) = match meta.unix_mode {
            Some(mode) => (UNIX_HOST | 20, mode << 16),
            None => (20, 0),
        };
        let mut c = Vec::new();
        c.extend(0x0201_4b50u32.to_le_bytes());
        c.extend(made_by.to_le_bytes());
        c.extend(20u16.to_le_bytes());
        c.extend(flags.to_le_bytes());
        c.extend(body.method.to_le_bytes());
        c.extend(DOS_TIME.to_le_bytes());
        c.extend(DOS_DATE.to_le_bytes());
        c.extend(body.crc.to_le_bytes());
        c.extend(csize.to_le_bytes());
        c.extend(size.to_le_bytes());
        c.extend(name_len.to_le_bytes());
        c.extend([0u8; 8]); // extra, comment, disk, internal attributes
        c.extend(external.to_le_bytes());
        c.extend(offset.to_le_bytes());
        self.records.push(c.clone());
        self.central.extend(c);
        self.central.extend(name);
        self.count += 1;
        self
    }

    /// Another central record for entry `index`'s data, under a new name: the
    /// overlapping-entries bomb, where one compressed block is extracted as
    /// many files and the stored size counts it once per entry.
    pub fn alias(&mut self, name: &[u8], index: usize) -> &mut Self {
        let mut c = self.records[index].clone();
        c[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        self.central.extend(c);
        self.central.extend(name);
        self.count += 1;
        self
    }

    pub fn finish(mut self) -> Vec<u8> {
        let cd_offset = self.out.len() as u32;
        let cd_size = self.central.len() as u32;
        self.out.append(&mut self.central);
        self.out.extend(0x0605_4b50u32.to_le_bytes());
        self.out.extend([0u8; 4]);
        self.out.extend(self.count.to_le_bytes());
        self.out.extend(self.count.to_le_bytes());
        self.out.extend(cd_size.to_le_bytes());
        self.out.extend(cd_offset.to_le_bytes());
        self.out.extend(0u16.to_le_bytes());
        self.out
    }
}

/// A ustar archive; `kind` is the type flag (`b'0'` file, `b'1'` hard link,
/// `b'2'` symlink, `b'5'` folder).
pub struct Tar {
    out: Vec<u8>,
}

impl Tar {
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }

    pub fn add(&mut self, name: &str, kind: u8, link: &str, data: &[u8]) -> &mut Self {
        let mut h = [0u8; 512];
        put(&mut h[0..100], name.as_bytes());
        put(&mut h[100..108], b"0000644\0");
        put(&mut h[108..116], b"0000000\0");
        put(&mut h[116..124], b"0000000\0");
        put(
            &mut h[124..136],
            format!("{:011o}\0", data.len()).as_bytes(),
        );
        put(
            &mut h[136..148],
            format!("{:011o}\0", 1_792_756_800u64).as_bytes(),
        );
        h[156] = kind;
        put(&mut h[157..257], link.as_bytes());
        put(&mut h[257..265], b"ustar\x0000");
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        put(&mut h[148..156], format!("{sum:06o}\0 ").as_bytes());
        self.out.extend(h);
        self.out.extend(data);
        let pad = (512 - data.len() % 512) % 512;
        self.out.extend(std::iter::repeat_n(0u8, pad));
        self
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.out.extend([0u8; 1024]);
        self.out
    }
}

fn put(field: &mut [u8], value: &[u8]) {
    field[..value.len()].copy_from_slice(value);
}
