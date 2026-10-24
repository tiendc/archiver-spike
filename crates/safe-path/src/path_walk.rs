//! The path-based walk `Destination` used before folder handles, kept behind
//! the `path-walk` feature only to measure one against the other: every
//! folder on the way to a file is looked at by path, without following
//! links, for every file.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::destination::Unsafe;
use crate::name::RelPath;

pub(crate) fn create_file(root: &Path, path: &RelPath) -> Result<File, Unsafe> {
    let full = prepare(root, path)?;
    Ok(OpenOptions::new().write(true).create_new(true).open(full)?)
}

pub(crate) fn create_dir(root: &Path, path: &RelPath) -> Result<PathBuf, Unsafe> {
    let full = prepare(root, path)?;
    step_into(&full, path)?;
    Ok(full)
}

pub(crate) fn prepare(root: &Path, path: &RelPath) -> Result<PathBuf, Unsafe> {
    let mut full = root.to_path_buf();
    let parts = path.parts();
    for part in &parts[..parts.len() - 1] {
        full.push(part);
        step_into(&full, path)?;
    }
    full.push(&parts[parts.len() - 1]);
    if let Ok(meta) = fs::symlink_metadata(&full)
        && meta.file_type().is_symlink()
    {
        return Err(Unsafe::ThroughLink(path.to_string()));
    }
    Ok(full)
}

fn step_into(dir: &Path, for_path: &RelPath) -> Result<(), Unsafe> {
    for _ in 0..2 {
        match fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Unsafe::ThroughLink(for_path.to_string()));
            }
            Ok(meta) if meta.is_dir() => return Ok(()),
            Ok(_) => return Err(Unsafe::NotAFolder(for_path.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(dir) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            },
            Err(e) => return Err(e.into()),
        }
    }
    Err(Unsafe::NotAFolder(for_path.to_string()))
}
