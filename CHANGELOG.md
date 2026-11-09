# Changelog

## [0.7.0] — 2026-11-09

### Fixed

- **`merge` buffered every chunk in memory** before writing: merging a 1 GB
  archive peaked at **1.08 GB RSS**. Chunks are now streamed straight from the
  tar/zip entry to the output, one record at a time — same result, **4.8 MB**
  peak. This was the last place where memory still scaled with file size.

### Changed

- Split I/O buffers are capped (4 MB per chunk, 16 MB for the reader) instead of
  inheriting the whole `--buf-mb` budget. A 1 GB / 8-chunk split dropped from
  205 MB to 88 MB peak RSS and got slightly faster.

### Measured (release build, 1 GB / 19 M rows, M1 Pro)

| Operation | Time | Peak RSS |
|---|---|---|
| `index` | 0.54 s | 10.9 MB |
| `preview --start-row 18000000` | 0.49 ms | 3.5 MB |
| `split -n 8` | 2.42 s | 88 MB |
| `merge` | 1.05 s | 4.8 MB |
| `verify` | byte-identical | — |

## [0.6.0] — 2026-11-09

### Added

- **Sparse row index** (`crates/core/src/index.rs`): a sidecar `.idx` storing the
  byte offset of every 1000th record, plus `csv-splitter index` to build it.
  `preview --start-row N` now seeks through the index instead of scanning, so
  jumping to row 1,500,000 of an 87 MB file takes ~200 µs and ~5 MB of RAM.
- Index format is versioned (`CSI1`), written to a temp file and renamed, and
  validated against the file length before use; a stale index is rebuilt
  automatically.

### Notes

- Measured on the 1.6M-row / 87 MB sample: index build 0.53 s at 11.8 MB peak
  RSS, 13 KB on disk; random access 211 µs.

## [0.5.0] — 2026-11-09

### Added

- CLI progress now shows percentage, throughput and ETA for long phases
  (`  46% · 83.4 MB/s · ETA 1s`), throttled to four updates a second so CI logs
  stay readable. `--quiet` still silences everything but errors.

## [0.4.0] — 2026-11-09

### Added

- `split --by-column <name>`: one file per distinct value of a column, e.g.
  `--by-column city` gives `orders_CDMX.csv`, `orders_Puebla.csv`, …
  Implemented as external-sort + sequential grouping, so only one writer is open
  no matter how many values the column has. Values are sanitised for file names
  (`New York` → `New_York`, empty → `_empty`, case-insensitive duplicates get a
  suffix), and a cardinality guard fails the job instead of creating thousands
  of files.
- `--by-column` and `--by-size` are mutually exclusive; both are rejected with a
  usage error rather than silently ignoring one.

### Changed

- Archive packing and the scratch-directory guard moved into `pack.rs` and
  `tempdir.rs`, shared by split and partition.

## [0.3.0] — 2026-11-09

### Added

- `split --by-size <size>`: split by target chunk size (`100MB`, `1.5GiB`, `512k`, …)
  instead of a fixed chunk count. The chunk count is derived from
  `ceil(csv_bytes / target)`; a chunk may overshoot the target by at most one
  record. `-n` remains the default and keeps exact record counts per chunk.

### Changed

- The planning pass now reports CSV size as well as record count, so the job
  log states which strategy produced how many chunks.
- Chunk writers report their record counts back; a job fails loudly if the
  written total does not match the scanned total, instead of trusting the plan.

## [0.2.0] — 2026-11-09

### Added

- **CLI** (`crates/cli`, binary `csv-splitter`): `split`, `merge`, `preview`,
  `stats`, `extract`, `verify`. Same core pipeline as the desktop app.
- **`merge`**: re-assemble chunks from tar / tar.gz / tar.zst / tar.bz2 / tar.xz /
  zip (or a directory) into a single CSV, keeping only the first header.
- **`verify`**: split → merge → byte compare, printing whether a job is lossless.
- Tests: 37 across core (unit + round-trip integration) and the CLI (black box).
- `LICENSE` file — the README was already linking to it.

### Fixed

- Splitting counted and cut **physical lines**: a quoted field containing a
  newline was torn across two chunks and preview/stats shifted every later
  column. Reading is now record-aware (`RecordReader`), including across buffer
  boundaries and escaped quotes.
- `-f zip` produced raw CSV bytes named `.csv.zip` inside a tar — no zip at all.
  Representation is now a real archive, and merge sniffs the container by magic
  bytes rather than trusting the extension.
- `--buf-mb` was allocated **per chunk** (64 MB × N). It is now a single shared
  budget, so a 10-way split no longer needs ~700 MB of buffers.
- `splits = 0` panicked with a divide-by-zero; `splits > records` created empty
  chunks. Both are rejected/handled, and the remainder is distributed so chunk
  sizes differ by at most one record.
- The scratch directory leaked on any failure path (now an RAII guard), and two
  concurrent jobs could share a temp directory when started in the same
  millisecond.
- A leading UTF-8 BOM leaked into the first column name; row-count estimates
  were computed from the first three (often short) rows.
- External sort also cut multi-line records.

### Changed

- CI runs `cargo test --workspace` and clippy with `--all-targets -D warnings`;
  the GUI's 36 `f64 → f32` inference warnings are fixed, so the pipeline is
  green on current stable Rust again.
- Workspace version bumped to 0.2.0.

## [0.1.0]

Initial release: split + compress pipeline, preview, column stats, decompress,
bilingual GUI.
