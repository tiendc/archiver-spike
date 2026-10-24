//! The "downloaded from the Internet" mark, carried from an archive to what
//! comes out of it.
//!
//! Windows keeps it in an alternate data stream, `file:Zone.Identifier`
//! (the Mark-of-the-Web); macOS in the `com.apple.quarantine` attribute.
//! Without it on the extracted files, SmartScreen and Gatekeeper never see
//! that `setup.exe` came from a stranger (CVE-2025-0411 was 7-Zip losing it
//! for an archive inside an archive). Linux has no such mark.

use std::io;
use std::path::Path;

/// The mark an archive carries, ready to put on its files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Origin {
    mark: Option<Vec<u8>>,
}

impl Origin {
    /// No mark: the archive was made here, or the system does not keep one.
    pub fn local() -> Self {
        Self::default()
    }

    pub fn is_marked(&self) -> bool {
        self.mark.is_some()
    }

    /// Reads the mark on `archive`. A missing mark is not an error.
    pub fn of(archive: &Path) -> io::Result<Self> {
        Ok(Self {
            mark: read_mark(archive)?,
        })
    }

    /// Puts the mark on an extracted file (also on a nested archive, so it
    /// keeps travelling). Does nothing when there is no mark.
    pub fn apply(&self, file: &Path) -> io::Result<()> {
        match &self.mark {
            Some(mark) => write_mark(file, mark),
            None => Ok(()),
        }
    }
}

#[cfg(windows)]
fn stream_of(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(":Zone.Identifier");
    s.into()
}

#[cfg(windows)]
fn read_mark(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(stream_of(path)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
fn write_mark(path: &Path, mark: &[u8]) -> io::Result<()> {
    std::fs::write(stream_of(path), mark)
}

#[cfg(target_os = "macos")]
const QUARANTINE: &str = "com.apple.quarantine";

#[cfg(target_os = "macos")]
fn read_mark(path: &Path) -> io::Result<Option<Vec<u8>>> {
    xattr::get(path, QUARANTINE)
}

#[cfg(target_os = "macos")]
fn write_mark(path: &Path, mark: &[u8]) -> io::Result<()> {
    xattr::set(path, QUARANTINE, mark)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_mark(_: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn write_mark(_: &Path, _: &[u8]) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn the_mark_travels_to_extracted_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let archive = tmp.path().join("download.zip");
        let file = tmp.path().join("setup.exe");
        std::fs::write(&archive, b"PK").expect("archive");
        std::fs::write(&file, b"MZ").expect("file");

        assert!(!Origin::of(&archive).expect("read").is_marked());
        let mark: &[u8] = if cfg!(windows) {
            b"[ZoneTransfer]\r\nZoneId=3\r\n"
        } else {
            b"0081;66f1a2b3;Safari;"
        };
        write_mark(&archive, mark).expect("mark the archive");

        let origin = Origin::of(&archive).expect("read");
        assert!(origin.is_marked());
        origin.apply(&file).expect("apply");
        assert_eq!(read_mark(&file).expect("read").as_deref(), Some(mark));
    }
}
