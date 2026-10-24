//! A small extractor for ZIP and TAR built on spike-safe-path: what the
//! product's would do, minus the user interface. Every name goes through
//! `sanitize`, every file through `Destination` and `Budget`, links are
//! placed last, and the download mark is carried over.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use anyhow::Result;
use spike_safe_path::origin::Origin;
use spike_safe_path::{Bomb, Budget, Destination, RelPath, Unsafe, Verdict, sanitize};

use crate::report::shown;

/// Entries that share their data: the only reason to do that is a bomb.
const OVERLAP: &str = "entries share the same data (an overlapping zip bomb)";

/// What happened to each entry.
#[derive(Default, Debug)]
pub struct Outcome {
    pub written: Vec<String>,
    pub renamed: Vec<(String, String)>,
    pub refused: Vec<(String, String)>,
    pub stopped: Option<String>,
}

enum Kind {
    Dir,
    File,
    Symlink(String),
    HardLink(String),
}

/// One entry, whatever the format.
struct Item<'r> {
    stored: String,
    kind: Kind,
    stored_size: u64,
    size: u64,
    data: &'r mut dyn Read,
}

struct Run<'a> {
    dest: &'a Destination,
    budget: &'a mut Budget,
    origin: &'a Origin,
    out: Outcome,
    deferred: Vec<(RelPath, Kind)>,
}

impl Run<'_> {
    /// Returns false once the extraction has to stop.
    fn take(&mut self, item: Item<'_>) -> bool {
        let verdict = sanitize(&item.stored);
        let rel = match &verdict {
            Verdict::Rejected(reason) => {
                self.out
                    .refused
                    .push((shown(&item.stored), reason.to_string()));
                return true;
            }
            Verdict::Renamed(p, _) => {
                self.out.renamed.push((shown(&item.stored), p.to_string()));
                p.clone()
            }
            Verdict::Keep(p) => p.clone(),
        };
        if let Err(bomb) = self.budget.start_entry(item.stored_size) {
            self.out.stopped = Some(bomb.to_string());
            return false;
        }
        match item.kind {
            Kind::Dir => {
                if let Err(e) = self.dest.create_dir(&rel) {
                    self.out.refused.push((rel.to_string(), e.to_string()));
                }
            }
            kind @ (Kind::Symlink(_) | Kind::HardLink(_)) => self.deferred.push((rel, kind)),
            Kind::File => match self.dest.create_file(&rel) {
                Ok(mut file) => {
                    let mut metered = self.budget.meter(item.data, Some(item.size));
                    match io::copy(&mut metered, &mut file) {
                        Ok(_) => {
                            let full = self.dest.root().join(rel.to_path_buf());
                            if let Err(e) = self.origin.apply(&full) {
                                self.out
                                    .refused
                                    .push((rel.to_string(), format!("mark: {e}")));
                            }
                            self.out.written.push(rel.to_string());
                        }
                        Err(e) => {
                            drop(file);
                            let _ = std::fs::remove_file(self.dest.root().join(rel.to_path_buf()));
                            if let Some(bomb) = Bomb::from_io(&e) {
                                self.out.stopped = Some(format!("{rel}: {bomb}"));
                                return false;
                            }
                            self.out.refused.push((rel.to_string(), e.to_string()));
                        }
                    }
                }
                Err(e) => self.out.refused.push((rel.to_string(), e.to_string())),
            },
        }
        true
    }

    /// Links go last: by then every file is written, so none can be written
    /// through a link the archive placed.
    fn finish(mut self) -> Outcome {
        for (rel, kind) in std::mem::take(&mut self.deferred) {
            let result = match &kind {
                Kind::Symlink(target) => self.dest.check_link(&rel, target).and_then(|t| {
                    let at = self.dest.prepare(&rel)?;
                    make_symlink(&t, &at)?;
                    Ok(format!("{rel} → {target}"))
                }),
                Kind::HardLink(target) => self.dest.check_hard_link(&rel, target).and_then(|t| {
                    let at = self.dest.prepare(&rel)?;
                    std::fs::hard_link(&t, &at)?;
                    Ok(format!("{rel} ⇒ {target}"))
                }),
                Kind::Dir | Kind::File => continue,
            };
            match result {
                Ok(done) => self.out.written.push(done),
                Err(e) => self.out.refused.push((rel.to_string(), e.to_string())),
            }
        }
        self.out
    }
}

#[cfg(unix)]
fn make_symlink(target: &Path, at: &Path) -> Result<(), Unsafe> {
    Ok(std::os::unix::fs::symlink(target, at)?)
}

#[cfg(not(unix))]
fn make_symlink(_: &Path, _: &Path) -> Result<(), Unsafe> {
    Err(Unsafe::Io(io::Error::other(
        "links are not created on this system in the spike",
    )))
}

pub fn zip(
    archive: &Path,
    dest: &Destination,
    budget: &mut Budget,
    origin: &Origin,
) -> Result<Outcome> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
    let mut run = Run {
        dest,
        budget,
        origin,
        out: Outcome::default(),
        deferred: Vec::new(),
    };
    if zip.has_overlapping_entries()? {
        run.out.stopped = Some(OVERLAP.into());
        return Ok(run.out);
    }
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let stored = String::from_utf8_lossy(entry.name_raw()).into_owned();
        let kind = if entry.is_dir() {
            Kind::Dir
        } else if entry.is_symlink() {
            let mut target = String::new();
            entry.read_to_string(&mut target)?;
            Kind::Symlink(target)
        } else {
            Kind::File
        };
        let (stored_size, size) = (entry.compressed_size(), entry.size());
        if !run.take(Item {
            stored,
            kind,
            stored_size,
            size,
            data: &mut entry,
        }) {
            break;
        }
    }
    Ok(run.finish())
}

pub fn tar(
    archive: &Path,
    dest: &Destination,
    budget: &mut Budget,
    origin: &Origin,
) -> Result<Outcome> {
    let mut tar = tar::Archive::new(File::open(archive)?);
    let mut run = Run {
        dest,
        budget,
        origin,
        out: Outcome::default(),
        deferred: Vec::new(),
    };
    for entry in tar.entries()? {
        let mut entry = entry?;
        let stored = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let link = || {
            entry
                .link_name_bytes()
                .map(|l| String::from_utf8_lossy(&l).into_owned())
                .unwrap_or_default()
        };
        let t = entry.header().entry_type();
        let kind = if t.is_dir() {
            Kind::Dir
        } else if t.is_symlink() {
            Kind::Symlink(link())
        } else if t.is_hard_link() {
            Kind::HardLink(link())
        } else {
            Kind::File
        };
        let size = entry.size();
        if !run.take(Item {
            stored,
            kind,
            stored_size: size,
            size,
            data: &mut entry,
        }) {
            break;
        }
    }
    Ok(run.finish())
}

/// Reads a ZIP's data through the budget without writing it anywhere: for
/// bombs, where the point is how soon it stops.
pub fn drain_zip(
    archive: &Path,
    budget: &mut Budget,
    check_overlap: bool,
) -> Result<Option<String>> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
    if check_overlap && zip.has_overlapping_entries()? {
        return Ok(Some(format!("refused before reading: {OVERLAP}")));
    }
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if let Err(bomb) = budget.start_entry(entry.compressed_size()) {
            return Ok(Some(bomb.to_string()));
        }
        let size = entry.size();
        let mut metered = budget.meter(&mut entry, Some(size));
        if let Err(e) = io::copy(&mut metered, &mut io::sink()) {
            return Ok(Some(match Bomb::from_io(&e) {
                Some(bomb) => format!("stopped by the budget: {bomb}"),
                None => format!("stopped by the zip crate: {e}"),
            }));
        }
    }
    Ok(None)
}
