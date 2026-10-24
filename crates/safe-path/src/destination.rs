//! Creating files under one folder without being led out of it.

// With `path-walk` (measuring only) the folder-handle walk is left unused.
#![cfg_attr(feature = "path-walk", allow(dead_code))]

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cap_fs_ext::DirExt;
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};

use crate::name::{RelPath, sanitize};

/// Why something is not created.
#[derive(Debug, thiserror::Error)]
pub enum Unsafe {
    #[error("{0} would be written through a link")]
    ThroughLink(String),
    #[error("{0} is in the way and is not a folder")]
    NotAFolder(String),
    #[error("the link {link} points out of the folder ({target})")]
    LinkOutside { link: String, target: String },
    #[error("the hard link {link} points at {target}, which is not a file extracted here")]
    HardLinkOutside { link: String, target: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Folder handles kept open at most; past that the cache starts over, and a
/// folder is reached again from the root (macOS allows 256 open files by
/// default).
const KEPT_FOLDERS: usize = 64;

/// The folder an archive is extracted into.
///
/// Every folder on the way to a file is created and opened one at a time,
/// relative to the handle of the one above it and without following links,
/// and files are created relative to their folder's handle. So a link the
/// archive placed earlier (or that was already there) cannot carry a later
/// file somewhere else, not even one swapped in while extraction runs: what
/// was checked is the handle that is written through, not a path looked up
/// again. Folder handles are kept, so a file costs one create, not a lookup
/// per path component (each a handle open and close on Windows). Files are
/// created new, never opened: an existing name, or a link of that name, is an
/// error for the caller to turn into "Keep both" or "Skip".
pub struct Destination {
    root: PathBuf,
    dir: Dir,
    folders: Mutex<HashMap<PathBuf, Dir>>,
}

impl Destination {
    /// Creates the folder if needed. Its own path may go through links (the
    /// user chose it); nothing under it may.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        let root = fs::canonicalize(root.as_ref())?;
        let dir = Dir::open_ambient_dir(&root, ambient_authority())?;
        Ok(Self {
            root,
            dir,
            folders: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates a new, empty file at `path` for writing.
    pub fn create_file(&self, path: &RelPath) -> Result<File, Unsafe> {
        #[cfg(feature = "path-walk")]
        let created = crate::path_walk::create_file(&self.root, path);
        #[cfg(not(feature = "path-walk"))]
        let created = self.create_file_in_folder(path);
        created
    }

    /// Creates the folder `path` (and those above it). An existing folder is fine.
    pub fn create_dir(&self, path: &RelPath) -> Result<PathBuf, Unsafe> {
        #[cfg(feature = "path-walk")]
        let created = crate::path_walk::create_dir(&self.root, path);
        #[cfg(not(feature = "path-walk"))]
        let created = self.create_dir_in_folder(path);
        created
    }

    /// The full path of `path` with the folders above it in place; `path`
    /// itself is not created. For what has to be made by path (links).
    pub fn prepare(&self, path: &RelPath) -> Result<PathBuf, Unsafe> {
        #[cfg(feature = "path-walk")]
        let prepared = crate::path_walk::prepare(&self.root, path);
        #[cfg(not(feature = "path-walk"))]
        let prepared = self.prepare_in_folder(path);
        prepared
    }

    fn create_file_in_folder(&self, path: &RelPath) -> Result<File, Unsafe> {
        let parts = path.parts();
        let (name, folders) = parts.split_last().expect("a RelPath has a last part");
        self.with_folder(folders, path, |dir| {
            match dir.open_with(name, OpenOptions::new().write(true).create_new(true)) {
                Ok(file) => Ok(file.into_std()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && is_link(dir, name) => {
                    Err(Unsafe::ThroughLink(path.to_string()))
                }
                Err(e) => Err(e.into()),
            }
        })
    }

    fn create_dir_in_folder(&self, path: &RelPath) -> Result<PathBuf, Unsafe> {
        self.with_folder(path.parts(), path, |_| Ok(()))?;
        Ok(self.full(path))
    }

    fn prepare_in_folder(&self, path: &RelPath) -> Result<PathBuf, Unsafe> {
        let parts = path.parts();
        let (name, folders) = parts.split_last().expect("a RelPath has a last part");
        self.with_folder(folders, path, |dir| {
            if is_link(dir, name) {
                Err(Unsafe::ThroughLink(path.to_string()))
            } else {
                Ok(())
            }
        })?;
        Ok(self.full(path))
    }

    fn full(&self, path: &RelPath) -> PathBuf {
        let mut full = self.root.clone();
        for part in path.parts() {
            full.push(part);
        }
        full
    }

    /// Runs `then` on the handle of the folder `folders` (made of the parts
    /// of `for_path` above its last one), creating and opening the folders it
    /// does not have yet.
    fn with_folder<T>(
        &self,
        folders: &[String],
        for_path: &RelPath,
        then: impl FnOnce(&Dir) -> Result<T, Unsafe>,
    ) -> Result<T, Unsafe> {
        if folders.is_empty() {
            return then(&self.dir);
        }
        let mut cache = self.folders.lock().unwrap_or_else(|e| e.into_inner());
        let key: PathBuf = folders.iter().collect();
        if let Some(dir) = cache.get(&key) {
            return then(dir);
        }
        // Start from the deepest folder already open.
        let mut depth = folders.len();
        while depth > 0 && !cache.contains_key(&folders[..depth].iter().collect::<PathBuf>()) {
            depth -= 1;
        }
        let mut current = match depth {
            0 => self.dir.try_clone()?,
            _ => cache[&folders[..depth].iter().collect::<PathBuf>()].try_clone()?,
        };
        if cache.len() + folders.len() - depth > KEPT_FOLDERS {
            cache.clear();
        }
        for (i, part) in folders.iter().enumerate().skip(depth) {
            current = step_into(&current, part, for_path)?;
            cache.insert(folders[..=i].iter().collect(), current.try_clone()?);
        }
        then(&current)
    }

    /// Checks the target of a symbolic link the archive stores at `link`. It
    /// must be relative and, followed from the link's folder, stay inside
    /// the destination at every step (`a/../../x` leaves it even though it
    /// ends lower down). Returns the target with this system's separators.
    pub fn check_link(&self, link: &RelPath, target: &str) -> Result<PathBuf, Unsafe> {
        let outside = || Unsafe::LinkOutside {
            link: link.to_string(),
            target: target.to_owned(),
        };
        if target.is_empty()
            || target.contains('\0')
            || target.starts_with(['/', '\\'])
            || target.as_bytes().get(1) == Some(&b':')
        {
            return Err(outside());
        }
        let mut depth = link.parts().len() - 1;
        let mut out = PathBuf::new();
        for part in target.split(['/', '\\']) {
            match part {
                "" | "." => {}
                ".." => {
                    depth = depth.checked_sub(1).ok_or_else(outside)?;
                    out.push("..");
                }
                part => {
                    depth += 1;
                    out.push(part);
                }
            }
        }
        Ok(out)
    }

    /// Checks the target of a hard link (as tar stores them): a name in the
    /// same archive, already extracted here as a plain file.
    pub fn check_hard_link(&self, link: &RelPath, stored_target: &str) -> Result<PathBuf, Unsafe> {
        let outside = || Unsafe::HardLinkOutside {
            link: link.to_string(),
            target: stored_target.to_owned(),
        };
        let target = sanitize(stored_target)
            .path()
            .cloned()
            .ok_or_else(outside)?;
        let full = self.prepare(&target)?;
        match fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_file() => Ok(full),
            _ => Err(outside()),
        }
    }
}

/// Opens the folder `part` under `dir`, creating it if it is missing, and
/// never through a link.
fn step_into(dir: &Dir, part: &str, for_path: &RelPath) -> Result<Dir, Unsafe> {
    match dir.create_dir(part) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    match dir.open_dir_nofollow(part) {
        Ok(folder) => Ok(folder),
        Err(e) => match dir.symlink_metadata(part) {
            Ok(meta) if meta.file_type().is_symlink() => {
                Err(Unsafe::ThroughLink(for_path.to_string()))
            }
            Ok(meta) if !meta.is_dir() => Err(Unsafe::NotAFolder(for_path.to_string())),
            _ => Err(e.into()),
        },
    }
}

fn is_link(dir: &Dir, name: &str) -> bool {
    dir.symlink_metadata(name)
        .is_ok_and(|meta| meta.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::Verdict;

    fn rel(s: &str) -> RelPath {
        match sanitize(s) {
            Verdict::Keep(p) => p,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn files_and_folders_are_created_inside() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = Destination::new(tmp.path().join("out")).expect("dest");
        dest.create_file(&rel("a/b/c.txt")).expect("file");
        assert!(tmp.path().join("out/a/b/c.txt").is_file());
        dest.create_dir(&rel("a/b"))
            .expect("existing folder is fine");
        assert!(matches!(
            dest.create_file(&rel("a/b/c.txt")),
            Err(Unsafe::Io(_))
        ));
        assert!(matches!(
            dest.create_file(&rel("a/b/c.txt/d")),
            Err(Unsafe::NotAFolder(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_link() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        let dest = Destination::new(tmp.path().join("out")).expect("dest");
        std::os::unix::fs::symlink(&outside, dest.root().join("link")).expect("symlink");
        assert!(matches!(
            dest.create_file(&rel("link/pwned.txt")),
            Err(Unsafe::ThroughLink(_))
        ));
        assert!(matches!(
            dest.create_file(&rel("link")),
            Err(Unsafe::ThroughLink(_))
        ));
        assert_eq!(fs::read_dir(&outside).expect("read").count(), 0);
    }

    // The path walk refuses here rather than writing through the kept handle.
    #[cfg(all(unix, not(feature = "path-walk")))]
    #[test]
    fn a_folder_swapped_for_a_link_mid_extraction_leads_nowhere() {
        // What a path looked up again would follow: the folder is renamed
        // and a link to outside put in its place after it was created.
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        let dest = Destination::new(tmp.path().join("out")).expect("dest");
        dest.create_file(&rel("a/b/first.txt")).expect("file");
        fs::rename(dest.root().join("a"), dest.root().join("moved")).expect("rename");
        std::os::unix::fs::symlink(&outside, dest.root().join("a")).expect("symlink");
        // The kept handle is the folder that was checked, now `moved/b`.
        dest.create_file(&rel("a/b/second.txt")).expect("file");
        assert!(dest.root().join("moved/b/second.txt").is_file());
        assert_eq!(fs::read_dir(&outside).expect("read").count(), 0);
        // Reached afresh, the link is refused.
        let fresh = Destination::new(dest.root()).expect("dest");
        assert!(matches!(
            fresh.create_file(&rel("a/b/third.txt")),
            Err(Unsafe::ThroughLink(_))
        ));
        assert_eq!(fs::read_dir(&outside).expect("read").count(), 0);
    }

    #[cfg(not(feature = "path-walk"))]
    #[test]
    fn folders_past_the_kept_handles_are_reached_again() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = Destination::new(tmp.path().join("out")).expect("dest");
        let count = KEPT_FOLDERS * 3;
        for round in 0..2 {
            for i in 0..count {
                let name = format!("deep/{:03}/inner/{round}.txt", i);
                dest.create_file(&rel(&name)).expect("file");
            }
        }
        for i in 0..count {
            for round in 0..2 {
                let path = tmp
                    .path()
                    .join(format!("out/deep/{i:03}/inner/{round}.txt"));
                assert!(path.is_file(), "{}", path.display());
            }
        }
        assert!(dest.folders.lock().expect("lock").len() <= KEPT_FOLDERS);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_deep_in_the_tree_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        let dest = Destination::new(tmp.path().join("out")).expect("dest");
        dest.create_dir(&rel("a/b/c")).expect("folders");
        std::os::unix::fs::symlink(&outside, dest.root().join("a/b/c/d")).expect("symlink");
        assert!(matches!(
            dest.create_file(&rel("a/b/c/d/e/f.txt")),
            Err(Unsafe::ThroughLink(_))
        ));
        assert!(matches!(
            dest.prepare(&rel("a/b/c/d")),
            Err(Unsafe::ThroughLink(_))
        ));
        assert_eq!(fs::read_dir(&outside).expect("read").count(), 0);
    }

    #[test]
    fn link_targets_must_stay_inside() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = Destination::new(tmp.path()).expect("dest");
        let ok = |l: &str, t: &str| dest.check_link(&rel(l), t).is_ok();
        assert!(ok("a/link", "../b/file"));
        assert!(ok("a/b/link", "../../c"));
        assert!(ok("link", "a/./b"));
        assert!(!ok("link", "../outside"));
        assert!(!ok("a/link", "../../outside"));
        assert!(!ok("a/link", "x/../../../outside"));
        assert!(!ok("link", "/etc"));
        assert!(!ok("link", "C:\\Windows"));
        assert!(!ok("link", "\\\\server\\share"));
        assert!(!ok("link", ""));
    }

    #[test]
    fn hard_links_must_name_an_extracted_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = Destination::new(tmp.path()).expect("dest");
        dest.create_file(&rel("data/real.txt")).expect("file");
        assert!(dest.check_hard_link(&rel("copy"), "data/real.txt").is_ok());
        assert!(dest.check_hard_link(&rel("copy"), "/etc/passwd").is_err());
        assert!(dest.check_hard_link(&rel("copy"), "../x").is_err());
        assert!(dest.check_hard_link(&rel("copy"), "data/missing").is_err());
    }
}
