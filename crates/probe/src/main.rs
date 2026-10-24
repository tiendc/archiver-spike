//! Runs the spike's checks over the corpus (`cargo run -p spike-corpus`
//! first) and writes `results/report.md`.
//!
//! `probe [corpus]` runs everything; `SPIKE_SKIP_BENCH=1` leaves out the
//! speed test, `SPIKE_BENCH_MB` sizes it, `SPIKE_RAR_DIR` adds RAR samples.

mod bench;
mod checks;
mod extract;
mod report;
mod volumes;

use std::path::PathBuf;

use anyhow::Result;

fn main() -> Result<()> {
    let corpus = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "corpus".into()));
    let mut r = report::Report::default();
    r.line("# Archiver spike: results");
    r.line(format!(
        "\nMachine: {} {}, written by `probe` from the generated corpus.",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    checks::names(&corpus, &mut r)?;
    checks::hostile(&corpus, &mut r)?;
    checks::seven_zip(&corpus, &mut r)?;
    checks::rar(&mut r)?;
    if std::env::var_os("SPIKE_SKIP_BENCH").is_none() {
        bench::run(&mut r)?;
    }
    std::fs::create_dir_all("results")?;
    std::fs::write("results/report.md", r.finish())?;
    println!("\nwritten to results/report.md");
    Ok(())
}
