# rars decoder fixes: how they were found and measured

These were made in a vendored copy of rars 0.12.0 while the spike looked for
where rars' decoders were slow. They are now six pull requests to upstream,
each on its own branch of the fork <https://github.com/tiendc/rars>, and all
six together on its `perf-combined` branch. All of them were merged on
2026-10-24 (with follow-up fixes by the maintainer), and the spike now uses
upstream `master` until a release carries them:

| PR | Branch | Changes below |
|---|---|---|
| [bitplane/rars#89](https://github.com/bitplane/rars/pull/89) | `rar5-history-slack` | 1 |
| [bitplane/rars#90](https://github.com/bitplane/rars/pull/90) | `table-driven-huffman` | 2, 3 |
| [bitplane/rars#91](https://github.com/bitplane/rars/pull/91) | `inline-read-poller` | 4 |
| [bitplane/rars#92](https://github.com/bitplane/rars/pull/92) | `rar29-history-slack` | 5, 6 |
| [bitplane/rars#93](https://github.com/bitplane/rars/pull/93) | `crc32fast` | 7 |
| [bitplane/rars#94](https://github.com/bitplane/rars/pull/94) | `ppmd-state-arena` | 8, 9 |
| [bitplane/rars#96](https://github.com/bitplane/rars/pull/96) | `solid-directory-flag` | writer: directory headers in a solid run carry FHD_SOLID, as RAR writes them; without it 7-Zip could not extract solid RAR 2.0–4 archives with folders ([#95](https://github.com/bitplane/rars/issues/95)) |

What follows is the record kept while the copy was vendored (paths are
relative to that copy, `crates/rars/` upstream).

The RAR reader the spike checks against UnRAR, with fixes to where its
decoders were slow.

- **Upstream:** `rars` 0.12.0 on crates.io, <https://github.com/bitplane/rars>.
  License: Apache-2.0 (`COPYING`).
- **As published:** `git -C vendor/rars diff` against the first commit of the
  local repository there (0.12.0 exactly as on crates.io). The local
  repository is only a way to see the changes; it goes once they are sent.
- **How it is used:** the workspace's `Cargo.toml` puts it in place of the
  crates.io one (`[patch.crates-io]`) and keeps it out of the workspace
  (`exclude = ["vendor"]`).
- **Finding the changes:** each is marked `// archiver-spike:`;
  `grep -rn "archiver-spike:" vendor/rars/src`.
- **Checked by:** rars' own tests (`cargo test --release` in `vendor/rars`,
  2548 tests: a few changed as described below, one removed with the code it
  tested), the probe's comparison with
  UnRAR on 101 real RAR files (`SPIKE_RAR_DIR=corpus/rar`, the same outcome
  for every archive as before the changes), every file's size and CRC32
  against UnRAR on the three 197 MB, 8869-file solid archives below, and a
  repair from a recovery record after 32 KB of damage (read back by UnRAR as
  the original).

## Measured

`rar-perf read <archive> sink-handle` (decoding everything, CRC or hash
checked, nothing written), Apple M1 Pro, the same folder of 8869 files (half
real text, a third incompressible, the rest mostly zeros, 197 MB unpacked) as
three solid archives written by rars: RAR5; RAR4 at level 2, which is LZ
only, as WinRAR's default (m3) is; and RAR4 at rars' default level, where it
chose PPMd for the text (WinRAR does from m4). UnRAR 7.1 is the one the
`unrar` crate 0.5.8 vendors.

| | RAR5 | RAR4, LZ | RAR4, PPMd |
|---|---|---|---|
| rars 0.12.0 as published (`read_path`) | 55 MB/s | 134 MB/s | 24 MB/s |
| + change 1, RAR5 history slack | 84 | | |
| + change 2, table-driven Huffman (RAR5) | 149 | | |
| + change 4, and one file handle (`read_reader`) | 168 | | |
| + change 3, RAR4 bit reads and Huffman | | 146 | 24 |
| + change 5, RAR4 history slack | | 295 | 27 |
| + change 6, RAR4 matches copied in runs | | 316 | |
| + change 7, CRC-32 through crc32fast | | 406 | |
| changes 1–7 (with `decode_peeked` always inlined) | 163 | 423 | 28 |
| UnRAR 7.1 | 265 | 465 | 50 |

PPMd (changes 8 and 9) was measured later, on a busier machine, so against
the build just before them and UnRAR, the three run in turn five times on the
RAR4 PPMd archive:

| | decode only | extract to disk | peak memory |
|---|---|---|---|
| changes 1–7 | 25–26 MB/s | 23–24 MB/s | 193 MB |
| + changes 8 and 9 | 39–40 | 33 | 87 |
| UnRAR 7.1 | 46–49 | 32–34 | 56 |

On Linux (Debian 13, glibc 2.41, in Docker on the same M1 Pro, so aarch64,
not x86), the three archives, each build run in turn three times; every file
still matches UnRAR, and the 2549 tests pass (one more than on macOS: a test
only built on Linux):

| | RAR5 | RAR4, LZ | RAR4, PPMd | PPMd peak memory |
|---|---|---|---|---|
| rars 0.12.0 as published | 65 MB/s | 121–127 | 16–19 | 178 MB |
| changes 1–7 | 180–181 | 444–463 | 21–23 | 179 MB |
| changes 1–9 | 179–180 | 431–451 | 35–38 | 84 MB |
| UnRAR 7.1 | 221–230 | 428–444 | 35–49 | 56 MB |

Not measured yet: x86-64, and Windows.

Step by step (same busy machine, single runs, so roughly): the lazy mask and
the kept successor list 26 → 27, inline binary states 33, u32 successors 33
(no change), the state arena 40.

Extracting for real (every file written through the spike's path checks,
decoding and writing on two threads; UnRAR in a helper process sending files
over a pipe), the LZ RAR4 goes at 107 MB/s with rars and 95 MB/s with UnRAR.

What is left: in RAR5, about a tenth is BLAKE2sp (rars' writer stores BLAKE2sp
hashes; WinRAR only does when asked, `-htb`). In PPMd, nearly all the time is
now the model's own reads (contexts and their states, each a likely cache
miss), as in UnRAR; allocation is down from a sixth of the time to about 1%.
A context is still 32 bytes where UnRAR's is 12, and a state 12 where
UnRAR's is 6.

## Changes

1. **History slack (`src/codec/rar50.rs`, `ReaderState::remember_history`).**
   After every member of a solid archive the history was trimmed to exactly
   one dictionary, moving the whole dictionary (4 MiB here) to the front of
   the buffer: for 8869 small files, most of the decode time went to
   `memmove`. The history now keeps up to a quarter dictionary of slack, so a
   member that fits is appended and nothing moves; the tail moves once the
   slack is full, and storage still grows by doubling and is given back when
   a smaller dictionary follows. Matches never reach past `dictionary_size`
   (`copy_match` checks it), so the older bytes are never read. The test
   `buffered_reader_history_capacity_follows_the_active_dictionary` now checks
   the retained tail and a capacity of at most 1.25 dictionaries.
2. **Table-driven Huffman decoding for RAR5 (`src/codec/canonical.rs`,
   `Huffman::decode_peeked` and the `quick` table; `src/codec/rar50.rs`,
   `HuffmanState::decode`, `BitReader::peek15`, `BitReader::skip`).** Each
   symbol was decoded one bit at a time, a bounds-checked `read_bits(1)` per
   bit. Now the next 15 bits are peeked once: codes of up to 10 bits (nearly
   all) come from a 1024-entry table, longer ones from the canonical counts
   using the peeked bits. Errors are what bit-by-bit decoding gave: "needs
   more input" when the code runs past the end, "invalid code" when 15 real
   bits start none. Each table now holds 4 KiB more, charged to the same
   allowance; `table_and_checkpoint_allocations_are_admitted_and_released`
   counts it.
3. **RAR 2.9 (RAR4) bit reads and Huffman decoding (`src/codec/rar29.rs`,
   `BitReader::peek_bits`, `BitReader::peek15`, `Huffman::decode`).** Every
   bit read, of any width, looped over single bits; reads now take the bytes
   they span at once. Huffman decoding is table-driven as in change 2.
   `Huffman::decode_peeked` is `#[inline(always)]`: once both decoders called
   it, the compiler stopped inlining it into RAR5's loop, which cost RAR5 11%.
4. **The cancellation check is inlined (`src/read_control.rs`,
   `Poller::check`, `Poller::check_codec`).** Decoders call it once per
   symbol; without a cancellation token it is one test, which was costing a
   call (5% of the time).

5. **RAR4 history slack (`src/codec/rar29.rs`, `Reader29State::trim_history`,
   `Reader29State::copy_match`).** The output buffer was drained after every
   flush, moving the whole 4 MiB window each time: over half the time of the
   LZ archive. It is now drained only once a quarter window can go. Distances
   were refused only by whether their bytes had been drained yet; with the
   slack they may still be there, so `copy_match` now refuses a distance past
   RAR 2.9's 4 MiB window (or of zero) itself, which also makes the refusal
   independent of when trimming last ran. Five tests pinned the exact
   trimming (`base_offset`, buffer lengths, bytes at fixed positions) and now
   check what it is for: the data, the window retained and at most a quarter
   more, the match at exactly the window, stale filters dropped and future
   ones kept once trimming happens (`history_trimming_discards_stale_filters_and_keeps_future_filters`
   now trims a quarter window, and checks that 64 bytes do not), and the
   distance past the window refused.
6. **RAR4 matches copied in runs (`Reader29State::copy_match`).** A match was
   copied a byte at a time, each byte a position check, a bounds check and a
   budgeted push. Now room is made once and short matches (up to 64 bytes,
   nearly all of them) are copied in a plain loop; longer ones with
   `extend_from_within`, doubling for short distances. A `memmove` call per
   match was slower than the byte loop (168 MB/s against 295), hence the
   split. What does not fit before `output_size` still waits in
   `pending_match`; `raw_byte` had no other caller and is gone.
7. **CRC-32 through crc32fast (`src/crc32/mod.rs`, `update_raw`; a new
   dependency, `crc32fast` 1.5).** Every extracted byte is checksummed; the
   slicing-by-8 tables ran at about 2 GB/s, a sixth of the LZ RAR4 time.
   crc32fast uses the CPU's CRC instructions where there are any and tables
   where not; inputs under 64 bytes keep rars' tables.

8. **PPMd model storage (`src/codec/ppmd.rs`: `Context`, `Stats`,
   `StateArena`, `PpmdState::states`, `states_mut`, `push_state`,
   `truncate_states`, `rescale`; `Successor`).** Every context owned a `Vec`
   of its states: one allocation per context, most of them holding one state,
   all freed one by one at every model restart (a sixth of the decode time
   went to malloc, free and `madvise`), and a context was 56 bytes. Now a
   context is plain data of 32 bytes: one state is kept inline, as UnRAR
   keeps `OneState` inside the context; more live in one arena in blocks of
   2, 4, ..., 256 states, with per-size free lists, a larger free block halved
   for a smaller size, and a context that shrinks keeping the front of its
   block. A restart empties the arena and keeps its storage, so it frees
   nothing. `rescale` works in place instead of on a copy. Successors (a text
   position or a context index) and suffixes are `u32` (a state is 12 bytes
   instead of 24); `Successor::raw` and `Successor::context` refuse a value
   that does not fit rather than wrap. The model itself is unchanged: the
   simulated suballocator offsets still decide every restart, exactly as
   before; only where the states are kept differs. A context can no longer
   be given a 257th state (an error; the model never adds a symbol a context
   already has). The PPMd tests built contexts as literals with a `Vec` of
   states; they now name the states, sum and suffix through three test
   helpers (`context`, `push_test_context`, `push_raw_context`) and read
   states through `states` / `states_mut`, with the same assertions.
   `Buffer::insert` and `Buffer::remove` had no other caller and are gone,
   with the test of them (`limited_insert_preserves_reordered_storage_and_refuses_growth_atomically`).
9. **PPMd, smaller (`src/codec/ppmd.rs`).** The 256-byte escape mask was
   filled for every symbol; it is now filled only after the first context
   escapes, which most symbols never do. `create_successors` allocated its
   list of states to link on every call; the list is now kept in the model
   (`successor_scratch`). The suballocator's bucket tables are built at
   compile time instead of behind a `OnceLock` read on every allocation, and
   `create_successors` reads the found symbol once instead of once per
   suffix.

## Not changed, noticed

- `ArchiveSource::range_reader` opens the archive again for every member when
  it was read with `read_path` (7% of the time here). `read_reader` with one
  open file avoids it; `read_path` could keep its handle.
- BLAKE2sp is computed lane after lane; its eight lanes are independent.
- The decoded output of every member is a new allocation, freed after it is
  written (`madvise` shows in profiles).
