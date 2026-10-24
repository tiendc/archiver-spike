//! A stored name, checked and rewritten into a relative path.

use std::fmt;
use std::path::PathBuf;

use unicode_normalization::UnicodeNormalization;

/// The most components a path may have, and the longest it may be in bytes.
/// Real archives stay far below both; past them it is an attack on the
/// extractor or the file system.
const MAX_DEPTH: usize = 64;
const MAX_LEN: usize = 4096;
/// The longest component most file systems allow (NTFS, APFS, ext4), in bytes.
const MAX_COMPONENT: usize = 255;

/// What became of a stored name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Safe as stored.
    Keep(RelPath),
    /// Safe once changed; the changes say how, for the report the user sees.
    Renamed(RelPath, Vec<Change>),
    /// Not written at all.
    Rejected(Reason),
}

impl Verdict {
    /// The path to write, if there is one.
    pub fn path(&self) -> Option<&RelPath> {
        match self {
            Verdict::Keep(p) | Verdict::Renamed(p, _) => Some(p),
            Verdict::Rejected(_) => None,
        }
    }
}

/// Why a name is not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Reason {
    #[error("the name is empty")]
    Empty,
    #[error("the name starts at the root of the disk")]
    Absolute,
    #[error("the name names a drive or a network share")]
    DriveOrShare,
    #[error("the name climbs out of the folder (..)")]
    ParentDir,
    #[error("a part of the name is only dots or spaces")]
    DotsOnly,
    #[error("the name contains a NUL character")]
    Nul,
    #[error("a part of the name is longer than 255 bytes")]
    ComponentTooLong,
    #[error("the name is too long or too deep")]
    TooLong,
}

/// A change made to a name so it can be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A character no file system here can take (`<>"|?*`, a control
    /// character), replaced by `_`.
    InvalidChar(char),
    /// A `:`, which Windows reads as "write into a hidden stream of the
    /// file before it": replaced by `_`.
    StreamSeparator,
    /// A name Windows keeps for devices (`CON`, `NUL`, `COM1`…): `_` put in front.
    ReservedName(String),
    /// Dots or spaces at the end of a part, which Windows drops by itself.
    TrailingDotOrSpace,
    /// Letters stored decomposed (as macOS writes them), composed.
    Composed,
}

/// A path under the destination folder: never empty, never absolute, never
/// `..`, each part safe on Windows, macOS and Linux alike, composed (NFC).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelPath {
    parts: Vec<String>,
}

impl RelPath {
    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    /// The parts as a path for this system.
    pub fn to_path_buf(&self) -> PathBuf {
        self.parts.iter().collect()
    }

    /// The folder this path is in, if it is not at the top.
    pub fn parent(&self) -> Option<RelPath> {
        (self.parts.len() > 1).then(|| RelPath {
            parts: self.parts[..self.parts.len() - 1].to_vec(),
        })
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.parts.join("/"))
    }
}

/// Checks a name as stored in an archive (already decoded to text) and turns
/// it into a path under the destination folder.
///
/// Both `/` and `\` separate parts: archives made on Windows use `\`, and a
/// `\` kept as a character on macOS or Linux would become a separator again
/// the moment the folder is copied to Windows.
pub fn sanitize(stored: &str) -> Verdict {
    if stored.contains('\0') {
        // Some code truncates at NUL and some does not; the two would
        // disagree on which file this is.
        return Verdict::Rejected(Reason::Nul);
    }
    if stored.len() > MAX_LEN {
        return Verdict::Rejected(Reason::TooLong);
    }
    if stored.starts_with(['/', '\\']) {
        // Also `\\server\share` and `\\?\C:\`.
        return Verdict::Rejected(Reason::Absolute);
    }
    if has_drive_prefix(stored) {
        return Verdict::Rejected(Reason::DriveOrShare);
    }

    let mut changes = Vec::new();
    let mut parts = Vec::new();
    for raw in stored.split(['/', '\\']) {
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw == ".." {
            return Verdict::Rejected(Reason::ParentDir);
        }
        let composed: String = raw.nfc().collect();
        if composed != raw {
            push_once(&mut changes, Change::Composed);
        }
        // Windows drops trailing dots and spaces when it opens a path, so
        // ".. " and "..." mean something other than what they look like.
        let trimmed = composed.trim_end_matches(['.', ' ']);
        if trimmed.is_empty() {
            return Verdict::Rejected(if composed.trim_end_matches(' ') == ".." {
                Reason::ParentDir
            } else {
                Reason::DotsOnly
            });
        }
        if trimmed.len() != composed.len() {
            push_once(&mut changes, Change::TrailingDotOrSpace);
        }
        let mut part = replace_invalid(trimmed, &mut changes);
        if let Some(device) = reserved_device(&part) {
            changes.push(Change::ReservedName(device));
            part.insert(0, '_');
        }
        if part.len() > MAX_COMPONENT {
            return Verdict::Rejected(Reason::ComponentTooLong);
        }
        parts.push(part);
    }

    if parts.is_empty() {
        return Verdict::Rejected(Reason::Empty);
    }
    if parts.len() > MAX_DEPTH {
        return Verdict::Rejected(Reason::TooLong);
    }
    let path = RelPath { parts };
    if changes.is_empty() {
        Verdict::Keep(path)
    } else {
        Verdict::Renamed(path, changes)
    }
}

/// `C:`, `c:foo` (relative to the current folder of drive C, which is worse).
fn has_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn replace_invalid(part: &str, changes: &mut Vec<Change>) -> String {
    part.chars()
        .map(|c| match c {
            ':' => {
                push_once(changes, Change::StreamSeparator);
                '_'
            }
            '<' | '>' | '"' | '|' | '?' | '*' | '\u{1}'..='\u{1f}' | '\u{7f}' => {
                push_once(changes, Change::InvalidChar(c));
                '_'
            }
            c => c,
        })
        .collect()
}

/// The device name a part would open on Windows, whatever follows it:
/// `NUL`, `nul.txt` and `CON .tar.gz` all open the device.
fn reserved_device(part: &str) -> Option<String> {
    let stem = part.split('.').next().unwrap_or(part).trim_end_matches(' ');
    let upper = stem.to_uppercase();
    let is_device = matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.chars().count() == 4
        && upper
            .chars()
            .nth(3)
            .is_some_and(|d| d.is_ascii_digit() || matches!(d, '¹' | '²' | '³')));
    is_device.then_some(upper)
}

fn push_once(changes: &mut Vec<Change>, change: Change) {
    if !changes.contains(&change) {
        changes.push(change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(s: &str) -> String {
        match sanitize(s) {
            Verdict::Keep(p) => p.to_string(),
            other => panic!("{s:?} should be kept, got {other:?}"),
        }
    }

    fn renamed(s: &str) -> String {
        match sanitize(s) {
            Verdict::Renamed(p, _) => p.to_string(),
            other => panic!("{s:?} should be renamed, got {other:?}"),
        }
    }

    fn rejected(s: &str) -> Reason {
        match sanitize(s) {
            Verdict::Rejected(r) => r,
            other => panic!("{s:?} should be rejected, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_names_stay() {
        assert_eq!(kept("a/b/c.txt"), "a/b/c.txt");
        assert_eq!(
            kept("Bản vẽ mặt bằng tầng 1.pdf"),
            "Bản vẽ mặt bằng tầng 1.pdf"
        );
        assert_eq!(kept("dir\\file.txt"), "dir/file.txt");
        assert_eq!(kept("./a//b/./c"), "a/b/c");
        assert_eq!(kept(".hidden"), ".hidden");
        assert_eq!(kept("archive.tar.gz"), "archive.tar.gz");
        assert_eq!(kept("CONTRACT.pdf"), "CONTRACT.pdf");
        assert_eq!(kept("COM10.txt"), "COM10.txt");
    }

    #[test]
    fn climbing_out_is_rejected() {
        assert_eq!(rejected("../evil"), Reason::ParentDir);
        assert_eq!(rejected("a/../../evil"), Reason::ParentDir);
        assert_eq!(rejected("a\\..\\..\\evil"), Reason::ParentDir);
        assert_eq!(rejected("a/.. /evil"), Reason::ParentDir);
        assert_eq!(rejected("a/../b"), Reason::ParentDir);
        assert_eq!(rejected("a/.../b"), Reason::DotsOnly);
        assert_eq!(rejected("a/ . /b"), Reason::DotsOnly);
    }

    #[test]
    fn roots_drives_and_shares_are_rejected() {
        assert_eq!(rejected("/etc/passwd"), Reason::Absolute);
        assert_eq!(rejected("\\Windows\\x"), Reason::Absolute);
        assert_eq!(rejected("\\\\server\\share\\x"), Reason::Absolute);
        assert_eq!(rejected("\\\\?\\C:\\x"), Reason::Absolute);
        assert_eq!(rejected("C:\\Windows\\x"), Reason::DriveOrShare);
        assert_eq!(rejected("c:relative"), Reason::DriveOrShare);
    }

    #[test]
    fn streams_devices_and_bad_characters_are_renamed() {
        // CVE-2025-8088: "file:stream" writes into a hidden stream on NTFS.
        assert_eq!(
            renamed("doc.txt:Zone.Identifier"),
            "doc.txt_Zone.Identifier"
        );
        assert_eq!(renamed("a/b:c"), "a/b_c");
        assert_eq!(renamed("CON"), "_CON");
        assert_eq!(renamed("nul.txt"), "_nul.txt");
        assert_eq!(renamed("dir/Com1.log"), "dir/_Com1.log");
        assert_eq!(renamed("LPT²"), "_LPT²");
        assert_eq!(renamed("CON .tar.gz"), "_CON .tar.gz");
        assert_eq!(renamed("what?.txt"), "what_.txt");
        assert_eq!(renamed("tab\there"), "tab_here");
        assert_eq!(renamed("report. "), "report");
    }

    #[test]
    fn decomposed_letters_are_composed() {
        let nfd = "Ba\u{309}n ve\u{303}.pdf";
        match sanitize(nfd) {
            Verdict::Renamed(p, changes) => {
                assert_eq!(p.to_string(), "Bản vẽ.pdf");
                assert_eq!(changes, vec![Change::Composed]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nul_empty_and_huge_are_rejected() {
        assert_eq!(rejected("a\0b"), Reason::Nul);
        assert_eq!(rejected(""), Reason::Empty);
        assert_eq!(rejected("./"), Reason::Empty);
        assert_eq!(rejected(&"x".repeat(256)), Reason::ComponentTooLong);
        assert_eq!(rejected(&"a/".repeat(65)), Reason::TooLong);
    }
}
