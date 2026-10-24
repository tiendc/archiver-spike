//! The checks over the corpus: names, hostile archives, 7z, RAR.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use anyhow::{Context, Result};
use sevenz_rust2::{ArchiveReader, Password};
use spike_names::{Hint, guess};
use spike_safe_path::origin::Origin;
use spike_safe_path::{Budget, Destination, Limits};

use crate::extract::{self, Outcome};
use crate::report::{Report, yes};
use crate::volumes::{self, Joined};

const PASSWORD: &str = "spike-pass";

/// Do the old code pages come out right, guessed with and without a hint
/// from the user's country?
pub fn names(corpus: &Path, r: &mut Report) -> Result<()> {
    r.heading("File name encodings");
    let manifest = fs::read_to_string(corpus.join("encoding/expected.tsv"))?;
    let mut rows = Vec::new();
    let (mut plain_ok, mut hinted_ok, mut total) = (0, 0, 0);
    let (mut single_ok, mut single_hinted_ok) = (0, 0);
    for line in manifest.lines().filter(|l| !l.starts_with('#')) {
        let cols: Vec<&str> = line.split('\t').collect();
        let [file, real, tld, names] = cols[..] else {
            continue;
        };
        let expected: Vec<&str> = names.split('|').collect();
        let mut zip = zip::ZipArchive::new(File::open(corpus.join("encoding").join(file))?)?;
        let mut raw = Vec::new();
        let mut zip_crate_ok = true;
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i)?;
            raw.push(entry.name_raw().to_vec());
            zip_crate_ok &= expected
                .get(i)
                .is_some_and(|e| entry.name().is_ok_and(|n| n == *e));
        }
        let read_as = |hint: Hint<'_>| {
            let g = guess(raw.iter().map(Vec::as_slice), hint);
            let ok = raw
                .iter()
                .zip(&expected)
                .all(|(b, e)| g.charset.decode(b) == *e);
            (g, ok)
        };
        let (plain, ok_plain) = read_as(Hint::default());
        let hint = Hint {
            country_tld: (tld != "-").then_some(tld),
        };
        let (hinted, ok_hinted) = read_as(hint);
        // An archive of one file gives the detector a single short name.
        let single = guess([raw[0].as_slice()], Hint::default());
        let single_hinted = guess([raw[0].as_slice()], hint);
        let ok_single = single.charset.decode(&raw[0]) == expected[0];
        let ok_single_hinted = single_hinted.charset.decode(&raw[0]) == expected[0];
        total += 1;
        plain_ok += usize::from(ok_plain);
        hinted_ok += usize::from(ok_hinted);
        single_ok += usize::from(ok_single);
        single_hinted_ok += usize::from(ok_single_hinted);
        rows.push(vec![
            file.into(),
            real.into(),
            format!("{} {}", plain.charset.name(), yes(ok_plain)),
            format!("{} {}", hinted.charset.name(), yes(ok_hinted)),
            format!("{} {}", single.charset.name(), yes(ok_single)),
            format!("{} {}", single_hinted.charset.name(), yes(ok_single_hinted)),
            if tld == "-" {
                "–".into()
            } else {
                format!(".{tld}")
            },
            yes(zip_crate_ok),
        ]);
    }
    r.table(
        &[
            "Archive",
            "Written in",
            "All names, no hint",
            "All names, hint",
            "First name only",
            "First name, hint",
            "Hint",
            "zip crate alone",
        ],
        &rows,
    );
    r.line(format!(
        "Right with all names: {plain_ok}/{total} without a hint, {hinted_ok}/{total} with the user's country as a hint. \
         With one name only: {single_ok}/{total}, and {single_hinted_ok}/{total} with the hint. \
         \"zip crate alone\" is what an app gets by taking the crate's own reading of the names."
    ));
    Ok(())
}

fn outcome_rows(o: &Outcome) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for w in &o.written {
        rows.push(vec![w.clone(), "written".into(), String::new()]);
    }
    for (from, to) in &o.renamed {
        rows.push(vec![from.clone(), "renamed".into(), to.clone()]);
    }
    for (name, why) in &o.refused {
        rows.push(vec![name.clone(), "refused".into(), why.clone()]);
    }
    if let Some(s) = &o.stopped {
        rows.push(vec!["(extraction)".into(), "stopped".into(), s.clone()]);
    }
    rows
}

/// Everything under `top` that is not under `top/out`: what escaped.
fn escaped(top: &Path) -> Vec<String> {
    walkdir::WalkDir::new(top)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| !e.path().starts_with(top.join("out")))
        .map(|e| {
            e.path()
                .strip_prefix(top)
                .unwrap_or(e.path())
                .display()
                .to_string()
        })
        .collect()
}

pub fn hostile(corpus: &Path, r: &mut Report) -> Result<()> {
    r.heading("Hostile archives");
    let dir = corpus.join("hostile");
    let mut summary = Vec::new();
    for (file, is_tar) in [
        ("traversal.zip", false),
        ("symlink-escape.zip", false),
        ("links.tar", true),
    ] {
        // Extract two levels down, so an escape lands where it can be seen.
        let top = tempfile::tempdir()?;
        let dest = Destination::new(top.path().join("out/deep"))?;
        let mut budget = Budget::new(Limits::default());
        let archive = dir.join(file);
        let outcome = if is_tar {
            extract::tar(&archive, &dest, &mut budget, &Origin::local())?
        } else {
            extract::zip(&archive, &dest, &mut budget, &Origin::local())?
        };
        let escapes = escaped(top.path());
        r.line(format!("**{file}**"));
        r.table(&["Entry", "Result", "Detail"], &outcome_rows(&outcome));
        summary.push(vec![
            file.into(),
            yes(escapes.is_empty()),
            escapes.join(", "),
        ]);
    }
    r.line("Anything written outside the destination:");
    r.table(&["Archive", "Nothing escaped", "Escaped"], &summary);

    for (file, what, check_overlap) in [
        ("bomb.zip", "2 GiB of zeros in 2 MB", true),
        (
            "overlap.zip",
            "one 256 MiB block listed 400 times: 100 GiB from 256 KB",
            true,
        ),
        (
            "overlap.zip",
            "the same, with only the size and ratio limits (no overlap check)",
            false,
        ),
        ("lying-size.zip", "claims 1 KiB, holds 64 MiB", true),
    ] {
        let mut budget = Budget::new(Limits::default());
        let start = std::time::Instant::now();
        let stopped = extract::drain_zip(&dir.join(file), &mut budget, check_overlap)?;
        r.line(format!(
            "**{file}** ({what}): {} after {} MiB, {:.1} s",
            stopped.unwrap_or_else(|| "NOT stopped".into()),
            budget.written() >> 20,
            start.elapsed().as_secs_f64()
        ));
    }
    download_mark(&dir, r)
}

/// The mark set on a downloaded archive must reach a program inside an
/// archive inside it.
fn download_mark(dir: &Path, r: &mut Report) -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let download = tmp.path().join("nested.zip");
    fs::copy(dir.join("nested.zip"), &download)?;
    if !set_mark(&download)? {
        r.line("**nested.zip**: this system keeps no download mark; not checked here.");
        return Ok(());
    }
    let dest = Destination::new(tmp.path().join("out"))?;
    let mut budget = Budget::new(Limits::default());
    extract::zip(&download, &dest, &mut budget, &Origin::of(&download)?)?;
    let inner = dest.root().join("inner.zip");
    let inner_marked = Origin::of(&inner)?.is_marked();
    let dest2 = Destination::new(tmp.path().join("out2"))?;
    extract::zip(&inner, &dest2, &mut budget, &Origin::of(&inner)?)?;
    let exe_marked = Origin::of(&dest2.root().join("setup.exe"))?.is_marked();
    r.line(format!(
        "**nested.zip** (marked as downloaded): inner.zip marked {}, setup.exe inside it marked {}",
        yes(inner_marked),
        yes(exe_marked)
    ));
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_mark(path: &Path) -> Result<bool> {
    xattr::set(path, "com.apple.quarantine", b"0081;66f1a2b3;Safari;")?;
    Ok(true)
}

#[cfg(windows)]
fn set_mark(path: &Path) -> Result<bool> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n")?;
    Ok(true)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn set_mark(_: &Path) -> Result<bool> {
    Ok(false)
}

fn list_7z(path: &Path, password: &str) -> Result<Vec<String>, sevenz_rust2::Error> {
    let reader = ArchiveReader::open(path, Password::from(password))?;
    Ok(reader
        .archive()
        .files
        .iter()
        .map(|f| f.name().to_owned())
        .collect())
}

fn read_7z<R: Read + io::Seek>(source: R, password: &str) -> Result<u64, sevenz_rust2::Error> {
    let mut reader = ArchiveReader::new(source, Password::from(password))?;
    let mut total = 0;
    reader.for_each_entries(|_, data| {
        total += io::copy(data, &mut io::sink())?;
        Ok(true)
    })?;
    Ok(total)
}

fn result<T: std::fmt::Debug, E: std::fmt::Display>(r: &Result<T, E>) -> String {
    match r {
        Ok(v) => format!("ok: {v:?}"),
        Err(e) => format!("error: {e}"),
    }
}

pub fn seven_zip(corpus: &Path, r: &mut Report) -> Result<()> {
    r.heading("7z: hidden names and split archives");
    let dir = corpus.join("7z");
    let mut rows = Vec::new();
    for file in ["plain.7z", "names-visible.7z", "names-hidden.7z"] {
        let p = dir.join(file);
        let listed = list_7z(&p, "");
        let read_no_pw = read_7z(File::open(&p)?, "");
        let read_pw = read_7z(File::open(&p)?, PASSWORD);
        let wrong = list_7z(&p, "wrong-pass");
        rows.push(vec![
            file.into(),
            if listed.is_ok() {
                "visible".into()
            } else {
                "hidden".into()
            },
            result(&read_no_pw),
            result(&read_pw),
            result(&wrong.map(|v| v.len())),
        ]);
        if file == "names-hidden.7z" {
            r.line(format!(
                "names-hidden.7z listed with the password: {:?}",
                list_7z(&p, PASSWORD).ok()
            ));
        }
    }
    r.table(
        &[
            "Archive",
            "Names without password",
            "Read without password",
            "Read with password",
            "List with a wrong password",
        ],
        &rows,
    );
    if let Some(note) = cross_check_7zz(&dir)? {
        r.line(note);
    }

    let mut rows = Vec::new();
    for set in ["complete", "missing-middle", "missing-last"] {
        let d = dir.join("volumes").join(set);
        let first = fs::read_dir(&d)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .min()
            .context("no parts")?;
        let v = volumes::scan(&first)?;
        let verdict = match v.problem() {
            Some(problem) => format!("refused before reading: {problem}"),
            None => result(&read_7z(Joined::open(&v)?, "").map(|n| format!("{n} bytes extracted"))),
        };
        rows.push(vec![set.into(), v.parts.len().to_string(), verdict]);
    }
    r.table(&["Split set", "Parts found", "Result"], &rows);
    Ok(())
}

/// The real 7-Zip, if installed, must read what sevenz-rust2 wrote.
fn cross_check_7zz(dir: &Path) -> Result<Option<String>> {
    let Some(tool) = ["7zz", "7z"].into_iter().find(|t| which(t)) else {
        return Ok(Some(
            "7-Zip is not installed here, so these archives were not opened with it (`brew install sevenzip`, then run again)."
                .into(),
        ));
    };
    let mut notes = Vec::new();
    for file in ["names-visible.7z", "names-hidden.7z"] {
        let out = std::process::Command::new(tool)
            .args(["t", &format!("-p{PASSWORD}")])
            .arg(dir.join(file))
            .output()?;
        notes.push(format!("{file}: {}", yes(out.status.success())));
    }
    Ok(Some(format!("Opened with {tool}: {}", notes.join(", "))))
}

pub fn which(tool: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// RAR, read by UnRAR (RARLAB's C++, the reference) and by rars (pure Rust,
/// Apache-2.0, not derived from UnRAR), over real RAR files: SPIKE_RAR_DIR,
/// filled by `scripts/fetch-rar-samples.sh`. Each read runs in its own
/// process (`rar-one`) with a time limit, so a crash or a hang is a result.
pub fn rar(r: &mut Report) -> Result<()> {
    r.heading("RAR: UnRAR (C++) and rars (pure Rust)");
    let Ok(dir) = std::env::var("SPIKE_RAR_DIR") else {
        r.line("No RAR samples (run `sh scripts/fetch-rar-samples.sh`, then set SPIKE_RAR_DIR=corpus/rar).");
        return Ok(());
    };
    let helper = std::env::current_exe()?.with_file_name("rar-one");
    anyhow::ensure!(
        helper.exists(),
        "{} is missing: build it first (cargo build --release -p spike-probe)",
        helper.display()
    );
    let samples = rar_first_parts(Path::new(&dir));
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    let mut rows = Vec::new();
    let (mut t_unrar, mut t_rars) = (0.0, 0.0);
    for path in &samples {
        let (unrar, s1) = run_helper(&helper, "unrar", path)?;
        let (rars, s2) = run_helper(&helper, "rars", path)?;
        t_unrar += s1;
        t_rars += s2;
        let verdict = match (&unrar, &rars) {
            (Decoded::Ok(a, _), Decoded::Ok(b, _)) if a == b => "same files, same bytes",
            (Decoded::Ok(a, _), Decoded::Ok(b, _)) if same_file_data(a, b) => {
                "same file data, links shown differently"
            }
            (Decoded::Ok(..), Decoded::Ok(..)) => "both read, different results",
            (Decoded::Ok(..), _) => "only UnRAR reads it",
            (_, Decoded::Ok(..)) => "only rars reads it",
            (Decoded::Crashed(_), _) | (_, Decoded::Crashed(_)) => "a decoder crashed",
            (Decoded::TimedOut, _) | (_, Decoded::TimedOut) => "a decoder hung",
            _ => "both refuse it",
        };
        *counts.entry(verdict).or_default() += 1;
        if verdict != "same files, same bytes" {
            let name = path
                .strip_prefix(&dir)
                .unwrap_or(path)
                .display()
                .to_string();
            rows.push(vec![name, verdict.into(), unrar.short(), rars.short()]);
        }
    }
    let summary: Vec<Vec<String>> = counts
        .iter()
        .map(|(k, v)| vec![(*k).into(), v.to_string()])
        .collect();
    r.line(format!(
        "{} archives (split sets counted once), each read whole and checksummed:",
        samples.len()
    ));
    r.table(&["Outcome", "Archives"], &summary);
    r.line(format!(
        "Time for all of them: UnRAR {t_unrar:.1} s, rars {t_rars:.1} s (process start included)."
    ));
    r.line("Every archive where the two do not simply agree:");
    r.table(&["Archive", "Outcome", "UnRAR", "rars"], &rows);
    Ok(())
}

/// Whether two readings agree on every file with content. UnRAR reports a
/// link as an empty file; rars gives a RAR4 link's target as its content and
/// leaves RAR5 links out. A RAR5 file kept in versions is `name;1` in UnRAR
/// and `name` in rars. (A genuinely empty file is not compared here.)
fn same_file_data(unrar: &[String], rars: &[String]) -> bool {
    let parse = |lines: &[String]| -> Vec<(String, String)> {
        lines
            .iter()
            .filter_map(|l| {
                let (name, rest) = l.split_once('\t')?;
                let name = name
                    .rsplit_once(';')
                    .filter(|(_, v)| v.parse::<u32>().is_ok())
                    .map_or(name, |(n, _)| n);
                Some((name.to_owned(), rest.to_owned()))
            })
            .collect()
    };
    let (u, r) = (parse(unrar), parse(rars));
    u.iter()
        .filter(|(_, rest)| !rest.starts_with("0\t"))
        .all(|entry| r.contains(entry))
}

#[derive(PartialEq)]
enum Decoded {
    /// The files (name, size, CRC) and the password that opened them.
    Ok(Vec<String>, String),
    Refused(String),
    Crashed(String),
    TimedOut,
}

impl Decoded {
    fn short(&self) -> String {
        let cut = |s: &str| s.chars().take(90).collect::<String>();
        match self {
            Decoded::Ok(files, pw) => format!(
                "{} files{}",
                files.len(),
                if pw.is_empty() {
                    String::new()
                } else {
                    format!(" (password \"{pw}\")")
                }
            ),
            Decoded::Refused(e) => format!("refused: {}", cut(e)),
            Decoded::Crashed(e) => format!("CRASHED: {}", cut(e)),
            Decoded::TimedOut => "HUNG (killed after 20 s)".into(),
        }
    }
}

fn run_helper(helper: &Path, decoder: &str, archive: &Path) -> Result<(Decoded, f64)> {
    let start = std::time::Instant::now();
    let mut child = std::process::Command::new(helper)
        .args([decoder])
        .arg(archive)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let limit = std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let secs = start.elapsed().as_secs_f64();
    let Some(status) = status else {
        return Ok((Decoded::TimedOut, secs));
    };
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines: Vec<&str> = stdout.lines().collect();
    let last = lines.pop().unwrap_or_default();
    let read = if !status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        Decoded::Crashed(format!(
            "{status} {}",
            err.lines().last().unwrap_or_default()
        ))
    } else if let Some(pw) = last.strip_prefix("ok ").or((last == "ok").then_some("")) {
        Decoded::Ok(lines.iter().map(|l| l.to_string()).collect(), pw.to_owned())
    } else {
        Decoded::Refused(last.strip_prefix("error ").unwrap_or(last).to_owned())
    };
    Ok((read, secs))
}

/// Every .rar under `dir`, but only the first present part of a split set.
fn rar_first_parts(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut all: Vec<std::path::PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rar"))
        .collect();
    all.sort();
    let mut seen = std::collections::HashSet::new();
    all.into_iter()
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            match name.rsplit_once(".part") {
                Some((stem, _)) => seen.insert(p.with_file_name(stem)),
                None => true,
            }
        })
        .collect()
}
