# Archiver spike

Code written to check what the archiver's design promises before the
product is built on it: hidden 7z file names, split archives with a part
missing, file names in old code pages, extraction that hostile archives
cannot lead astray, the "downloaded from the Internet" mark, and speed.
It is a spike: the names (`spike-*`) and the layout are not the product's.
`spike-safe-path` is the part meant to move into the product as it is.

```sh
cargo run -p spike-corpus             # writes corpus/ (hostile archives included)
cargo run --release -p spike-probe    # runs every check, writes results/report.md
```

`SPIKE_SKIP_BENCH=1` leaves out the speed test, `SPIKE_BENCH_MB=512` sizes
it. RAR needs real files: `sh scripts/fetch-rar-samples.sh` puts libarchive's
and unrar.rs's test archives in `corpus/rar`, then
`SPIKE_RAR_DIR=corpus/rar` compares UnRAR with rars on them. Build the
probe first (`cargo build --release -p spike-probe`): it runs `rar-one`
next to itself, one process per archive and decoder. With 7-Zip installed (`7zz` on PATH), the probe also opens
the 7z files with it and adds it to the speed table.

`.github/workflows/bench.yml` runs the tests on Linux, Windows and macOS and
times extraction on hosted runners (Windows x64 and ARM64 too, with 7-Zip,
and on Windows once more with Defender's real-time protection turned on):
`scripts/bench-data.py` writes the folder, `scripts/bench-ci.sh` the ways it
is read.

## Licenses

The spike's own code is GPL-3.0-or-later (`COPYING`). To compare against
the reference, the probe links the `unrar` crate, which builds RARLAB's
UnRAR source under its own license; that license is not GPL-compatible, so
nothing here is distributed as a binary, and the product does not link it.
The RAR samples `scripts/fetch-rar-samples.sh` downloads (libarchive's,
BSD-2; unrar.rs's) stay out of the repository.

## Crates

- `crates/safe-path`: stored names into safe relative paths (`sanitize`),
  a destination that never writes through a link (`Destination`), limits on
  what an extraction writes (`Budget`), and the download mark (`origin`).
- `crates/names`: guessing the code page of an archive's file names.
- `crates/zip-split`: split ZIP sets (`.z01` … `.zip`) read as one archive
  by the `zip` crate; its test sets are Info-ZIP and 7-Zip output of
  generated data.
- `crates/corpus`: writes the test archives, byte by byte where a normal
  writer would refuse (names with `..`, no UTF-8 flag, lying sizes, links).
- `crates/probe`: a small ZIP/TAR extractor on top of `safe-path`, the
  checks, and the speed test.

## What the spike found (macOS, Apple M1 Pro, 2026-10-23)

| Promise in the design | Result | What it means |
|---|---|---|
| 7z "Hide file names too" | Works: `sevenz-rust2` encrypts the header (`set_encrypt_header`, on by default). Without the password the list is refused (`PasswordRequired`), a wrong one gives `MaybeBadPassword`. 7-Zip 26.05 opens and tests them. | The option can ship. |
| Split 7z, "Part 3 is missing" | Works: parts are joined into one stream (`probe/src/volumes.rs`); a gap is named before reading, and a missing last part is found from the 7z start header, which gives the whole length. | Both "part N missing" and "the last part is missing" can be said up front. |
| RAR without UnRAR | [rars](https://crates.io/crates/rars) 0.12 (pure Rust, Apache-2.0, not derived from UnRAR) against UnRAR on 101 real RAR files (libarchive's test archives and unrar.rs's): **every valid archive gives the same files and bytes**, encrypted names, solid and split sets included; links are reported differently (UnRAR: empty files; rars: a RAR4 link's target as content, RAR5 links left out, and 4 symlinks dropped in one split set). The 9 that only UnRAR opens are fuzzing samples, 6 of which give UnRAR 0 files. No crash or hang in either. | rars can be the RAR reader: no licence exception, no C++, no helper process. Links need our own handling anyway (`safe-path`). |
| Extraction to disk, per file | The rars CLI writes a file in ~82 system calls (strace, Linux) where UnRAR uses ~18: `lstat` on every path component about four times, `canonicalize` five times, a private staging folder then hard link + unlink, a second open to set the time, and the archive reopened per member. Timed alone on hosted runners (`tools/fsbench` on the fork's `bench` branch, ms per 1000 files, minimal → all those steps): Windows 181 → 1005, Linux 28 → 92, macOS 97 → 463. On Windows every path lookup is a handle open and close (~15 µs), so the same design costs most there: that is the 3× against 7-Zip. Files created relative to kept folder handles (cap-std, openat-style) cost the same as the minimal pattern on all three (185, 28, 97) and cannot be sent through a link; the spike's `safe-path` lstat walk costs +44% on Windows. Defender on hosted runners is off, and turning it on changed nothing (the work folder is likely excluded), so its cost on users' machines is unmeasured. | The app's extractor: cap-std folder handles, times set on the open handle, the archive kept open, no per-file canonicalize or staging. Done for the spike: `safe-path`'s `Destination` now creates folders and files relative to kept folder handles (cap-std, never following a link; at most 64 handles kept), with tests for a folder swapped for a link mid-extraction and for links deep in the tree. Real extraction of the 8869-file RAR5 archive on the M1 (busy machine, in turn): 56–62 → 62–65 MB/s (UnRAR 68–71); RAR4 LZ 86 → 94. |
| Extraction on Windows, hosted runners (Bench workflow) | 5000 files, ~200 MB. With the runner's Defender as shipped (real-time off on x64; both runners exclude all of C:\ and D:\), extraction is disk-bound and every tool lands at the same rate: x64 RAR5 rars 74–79 MB/s, UnRAR (through the spike's writer) 72–88, 7-Zip 81–97; RAR4 LZ rars 96–116, 7-Zip 76–84. With the exclusions removed and real-time protection on, Defender sets the pace for everyone: 8–11 MB/s on x64 (rars, UnRAR, 7-Zip alike), 9–15 on ARM64 (7-Zip there is the x64 build, emulated), about 3.5 ms per file; decoding alone does not change. Folder handles and the old path walk are within noise of each other in all of these. | Under Defender, scanning sets the pace. Closing files on 2, 4 or 8 writer threads (`rar-perf … rars-disk-pool<N>`) does not move it: 9–14 MB/s on x64 and 13–15 on ARM64 for every count, as for 7-Zip and UnRAR; Defender's scanning is the bottleneck, not the order files are closed in. Without scanning the pool gains little (ARM64 RAR4 LZ 72–74 → 83–84 with 8 writers, M1 106 → 144 with 4, Linux none). Keep one writer thread; nothing in the extractor itself gets past Defender. |
| Split ZIP (`.z01` … `.zip`) | `zip` 9 refuses multi-disk archives ("Multi-disk ZIP files are not supported"), even with the parts joined. `crates/zip-split` joins the parts and presents the central directory rewritten for one disk (offsets into the joined stream, disk numbers 0, end records rebuilt, ZIP64 included), so `zip` reads it unchanged. Tested on Info-ZIP 3.0 sets: deflate, stored, a 1501-entry directory spanning four parts, ZIP64 end records; every file matches 7-Zip's extraction. 7-Zip's `.zip.001` pieces are only joined. Info-ZIP's own merge (`zip -s 0`) wrote one bad CRC. | Use the adapter now; propose `ZipArchive::with_disks` upstream (zip2 #373 is about writing). |
| RAR speed | Same 197 MB, 8869-file folder as solid RAR5, RAR4 (LZ, like WinRAR's default) and RAR4 with PPMd. rars 0.12.0: 55, 134 and 24 MB/s; UnRAR: 265, 465 and 50. Profiling found the history trimmed after every file (a 4 MiB move each time, in both RAR5 and RAR4), Huffman codes read a bit at a time, RAR4 matches copied a byte at a time, and table CRC-32. Fixed in rars (all rars tests pass, every file still matches UnRAR): **163, 423 and 28 MB/s**, so RAR5 is 3× faster and 1.6× from UnRAR, RAR4 LZ 3.2× faster and 1.1× from UnRAR. Then PPMd: each context owned a `Vec` of its states (malloc and free were a sixth of the time, a restart freed them one by one); now one state is inline and the rest in one arena. PPMd 25–26 → **39–40 MB/s** against UnRAR's 46–49 (run in turn, busy machine), 1.2× from UnRAR, and peak memory 193 → 87 MB (UnRAR 56). | rars can be the RAR reader. The fixes are sent upstream as six PRs ([bitplane/rars#89–#94](https://github.com/bitplane/rars/pulls?q=is%3Apr+author%3Atiendc)); until they are released the spike uses the fork's `perf-combined` branch (`docs/rars-fixes.md`). |
| Split RAR | Both read split sets. UnRAR (library mode) asks for each next part and the crate answers "stop" when it is missing; part names come from `Archive::nth_part`. | "Find part 3…" works by checking the expected names first. |
| "Try to repair" | UnRAR in library mode cannot repair. **rars can**, from the recovery record: on a RAR5 it wrote with 10% recovery, 4, 32 and 96 KB of damage were repaired byte-exact (checked by UnRAR) and 140 KB was refused as too much. On WinRAR's own recovery record (unrar.rs sample, every byte damaged in turn): 18 of 18 damages to file data repaired, none of the 91 damages to headers. For ZIP, repair is ours to write. | "Try to repair" can stay for RAR with a recovery record when the damage is in the data; damaged headers need more work in rars. |
| Names in old code pages | 11 of 11 code pages right (UTF-8 with and without the flag, decomposed UTF-8, windows-1258, Shift_JIS, GBK, Big5, EUC-KR, IBM866, windows-1252, DOS 437), also from a single name. DOS 437 needed a rule of our own (`dos_reads_better`); the detector does not know it. The zip crate's own reading got 3 of 11. | Detection is good enough to apply silently with "Do the file names look right?" as the escape. The corpus is generated: real archives (and pre-Unicode Vietnamese font encodings like VNI/TCVN3, which are not code pages at all) still need testing. |
| Hostile names and links | Nothing escaped from any test archive: `..`, `/`, `C:\`, `\\server`, NUL, `file:stream`, `CON`, trailing dots, a link pointing out with a file written "into" it, a hard link to `/etc/passwd`. | `spike-safe-path` can become the product's single gate. |
| Zip bombs | A 2 GiB-in-2 MB bomb stops on the ratio limit, but **only just** (1001 against a limit of 1000; Deflate tops out near 1032). An overlapping-entries bomb (100 GiB from 256 KB) is refused at once by `has_overlapping_entries`. A lying size is stopped by the zip crate itself. | Do not lean on the ratio: refuse overlapping entries, and limit the total to the free space on the disk. Keep the ratio as a question, not a wall. |
| Download mark | Carried from a marked archive to `inner.zip` and to `setup.exe` inside it (`com.apple.quarantine` here; `Zone.Identifier` on Windows, same code). | CVE-2025-0411's case is covered. Windows not run yet. |

Speed, 192 MB folder: half real text in many files (Rust sources from
`~/.cargo/registry`), 35% incompressible, 15% mostly zeros. "Extract" decodes
everything without writing it (7-Zip: `7zz t`).

| Format | Tool | Size | Compress | Extract |
|---|---|---|---|---|
| ZIP, Deflate 6 | zip crate (zlib-rs), 1 thread | 48% | 56 MB/s | 594 MB/s |
| ZIP, Deflate 6 | Info-ZIP `zip` (C), 1 thread | 48% | 53 MB/s | – |
| 7z, LZMA2 5 solid | sevenz-rust2, 10 threads | 41% (83.4 MB) | 32 MB/s | 1568 MB/s |
| 7z, LZMA2 5 solid | 7-Zip 26.05 `7zz -mx5 -mmt=on` | 41% (82.5 MB) | 11 MB/s | 458 MB/s |
| 7z made by 7-Zip | read by sevenz-rust2 | | | 352 MB/s |
| 7z made by sevenz-rust2 | read by 7-Zip | | | 2192 MB/s |
| tar.zst, level 3 | zstd crate (C), 10 threads | 45% | 500 MB/s | 2485 MB/s |
| RAR5 solid (made by rars, as test data) | read by UnRAR (C++) | 42% | – | 265 MB/s |
| RAR5 solid (same file) | read by rars 0.12.0 as published | 42% | – | 55 MB/s |
| RAR5 solid (same file) | read by rars with the fixes | 42% | – | 163 MB/s |

RAR, extracted for real (8869 files written through `spike-safe-path`,
`rar-perf read <archive> <how>`), against decoding alone:

| | RAR5, decode only | RAR5, to disk | RAR4 LZ, to disk | RAR4 PPMd, to disk |
|---|---|---|---|---|
| UnRAR in the same process | 276 MB/s | 80 MB/s | – | – |
| UnRAR in a helper process, files sent over a pipe | 249 MB/s | 88 MB/s | 95 MB/s | 35 MB/s (32–34 in the runs below) |
| rars (fixed), one thread | 172 MB/s | 65 MB/s | – | – |
| rars (fixed), decoding and writing on two threads | – | 77 MB/s | **107 MB/s** | 21 MB/s; **33 MB/s** with the PPMd fixes |

A helper process does not make UnRAR faster: the pipe costs about 10%. It
beats UnRAR in-process on disk only because decoding and writing then
overlap, which two threads do for rars as well. Writing 8869 files is most of
the time: on disk rars is 1.14× behind UnRAR for RAR5 and ahead of it for
RAR4 LZ. RAR4 with PPMd was decode-bound and 1.7× behind; with the PPMd
fixes it is level with UnRAR on disk (33 against 32–34 MB/s, the two run in
turn), though decoding alone is still 1.2× behind.
On Linux (aarch64 in Docker on the same machine, glibc) the fixes do the same:
RAR5 65 → 180 MB/s (UnRAR 221–230), RAR4 LZ 124 → 445 (UnRAR 428–444), PPMd
18 → 36 (UnRAR 35–49); `docs/rars-fixes.md` has the table. x86-64 and
Windows are not measured yet.

[unrar-ng](https://crates.io/crates/unrar-ng) 0.7.7, the maintained fork of
`unrar` (UnRAR 7.21 beta 1 instead of 7.1; `tools/ng-probe`, its own
workspace, as the two link the same C++ symbols): the same result as `unrar`
on all 101 samples; the same decode speed (per-file API: 275, 462 and 50 MB/s
on the three archives); its batch `extract_all`, which writes the files
itself, past `spike-safe-path`, 77, 82 and 33 MB/s to disk. On 60,000 small
files the per-file API took 0.65 s, like rars: the slowdown the fork was made
for (10× on 94,000 files) did not show with these archives, written by rars.

- The Rust Deflate matches the C `zip`.
- Each 7z reader opens the other's archives, hidden-name ones included.
- Reading the same archive, sevenz-rust2 is about 25% slower than 7-Zip.
  Writing, it is about 3× faster for a 1% larger file: it cuts LZMA2 into
  4 MiB chunks that compress (and later decompress) in parallel, where 7-Zip
  at `-mx5` keeps much larger blocks. That is a setting, not a gap.
- The first run used generated word salad for text: LZMA's match finder is
  at its worst on it (7-Zip: 45 s for 64 MB), so the speed test now uses real
  files and falls back to generated text only when it has to.

## Not done yet

- RAR at volume: the 101 samples are small test files; run rars over real
  downloads (`SPIKE_RAR_DIR` takes any folder) and fuzz it before trusting it.
- Run on Windows: the `Zone.Identifier` mark, reserved names, symlinks.
- ZIP repair; 7z and RAR through the same `Destination`/`Budget` path
  (only ZIP and TAR go through it so far); case-insensitive name collisions.
- The UI prototype (the Extract dialog in GPUI, the toolkit shellwing and
  authwing already use).
