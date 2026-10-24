//! Stopping an extraction that writes more than an archive honestly holds.

use std::io::{self, Read};

/// How much an extraction may write before it stops and asks.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Bytes in all, usually the free space on the destination disk.
    pub max_total: u64,
    pub max_entries: u64,
    /// Bytes written per byte stored. Deflate cannot pass about 1032:1, so
    /// a ZIP beyond that is lying or nested; LZMA over zeros (disk images)
    /// legitimately goes further, which is why passing it asks rather than fails.
    pub max_ratio: u64,
    /// The ratio is only looked at past this many bytes, so a small log
    /// that compresses 5000:1 is fine.
    pub ratio_after: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total: 256 << 30,
            max_entries: 1_000_000,
            max_ratio: 1_000,
            ratio_after: 1 << 30,
        }
    }
}

/// Why an extraction stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Bomb {
    #[error("the archive has more than {0} entries")]
    TooManyEntries(u64),
    #[error("extracting would write more than {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("the archive expands {ratio} times, far more than usual")]
    RatioTooHigh { ratio: u64 },
    #[error("an entry is larger than the archive says ({declared} bytes)")]
    LongerThanDeclared { declared: u64 },
}

impl Bomb {
    /// The bomb an I/O error from a [`Metered`] reader carries, if it is one.
    pub fn from_io(e: &io::Error) -> Option<&Bomb> {
        e.get_ref().and_then(|inner| inner.downcast_ref::<Bomb>())
    }
}

/// What one extraction has written so far.
pub struct Budget {
    limits: Limits,
    entries: u64,
    stored: u64,
    written: u64,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            entries: 0,
            stored: 0,
            written: 0,
        }
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    /// Counts an entry and the bytes it takes in the archive (0 when the
    /// format does not say per entry; then [`Budget::add_stored`] once).
    pub fn start_entry(&mut self, stored: u64) -> Result<(), Bomb> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(Bomb::TooManyEntries(self.limits.max_entries));
        }
        self.add_stored(stored);
        Ok(())
    }

    pub fn add_stored(&mut self, stored: u64) {
        self.stored = self.stored.saturating_add(stored);
    }

    /// Wraps an entry's decompressed data so reading it counts against the
    /// budget. `declared` is the size the archive gives the entry: a stream
    /// that runs past it is stopped there.
    pub fn meter<R: Read>(&mut self, inner: R, declared: Option<u64>) -> Metered<'_, R> {
        Metered {
            inner,
            budget: self,
            declared,
            read: 0,
        }
    }

    fn add_written(&mut self, n: u64) -> Result<(), Bomb> {
        self.written = self.written.saturating_add(n);
        if self.written > self.limits.max_total {
            return Err(Bomb::TooLarge {
                limit: self.limits.max_total,
            });
        }
        if self.written > self.limits.ratio_after {
            let ratio = self.written / self.stored.max(1);
            if ratio > self.limits.max_ratio {
                return Err(Bomb::RatioTooHigh { ratio });
            }
        }
        Ok(())
    }
}

/// An entry's data, counted as it is read.
pub struct Metered<'a, R> {
    inner: R,
    budget: &'a mut Budget,
    declared: Option<u64>,
    read: u64,
}

impl<R: Read> Read for Metered<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if let Some(declared) = self.declared
            && self.read > declared
        {
            return Err(io::Error::other(Bomb::LongerThanDeclared { declared }));
        }
        self.budget
            .add_written(n as u64)
            .map_err(io::Error::other)?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(r: &mut impl Read) -> io::Result<u64> {
        io::copy(r, &mut io::sink())
    }

    #[test]
    fn an_entry_longer_than_declared_stops() {
        let mut budget = Budget::new(Limits::default());
        budget.start_entry(10).expect("entry");
        let data = vec![0u8; 4096];
        let err = drain(&mut budget.meter(&data[..], Some(1024))).expect_err("stops");
        assert_eq!(
            Bomb::from_io(&err),
            Some(&Bomb::LongerThanDeclared { declared: 1024 })
        );
    }

    #[test]
    fn the_total_and_the_ratio_are_enforced() {
        let limits = Limits {
            max_total: 1 << 20,
            max_entries: 2,
            max_ratio: 100,
            ratio_after: 64 << 10,
        };
        let data = vec![0u8; 2 << 20];

        let mut budget = Budget::new(limits);
        budget.start_entry(1 << 20).expect("entry");
        let err = drain(&mut budget.meter(&data[..], None)).expect_err("too large");
        assert!(matches!(Bomb::from_io(&err), Some(Bomb::TooLarge { .. })));

        let mut budget = Budget::new(limits);
        budget.start_entry(100).expect("entry");
        let err = drain(&mut budget.meter(&data[..], None)).expect_err("ratio");
        assert!(matches!(
            Bomb::from_io(&err),
            Some(Bomb::RatioTooHigh { .. })
        ));

        let mut budget = Budget::new(limits);
        budget.start_entry(0).expect("1");
        budget.start_entry(0).expect("2");
        assert_eq!(budget.start_entry(0), Err(Bomb::TooManyEntries(2)));
    }
}
