# Changelog

## [0.14.0] — 2026-11-09

### Added

- **`/rows?sql=...`**: page through a query result instead of the raw file.
  `GET /rows?path=REL&sql=SELECT ...&start=500&count=50` returns rows 500..550 of
  the query — a table can scroll a filtered/sorted view without materialising it.
- `sql::page()` in core: `LIMIT count+1 OFFSET start`, so `has_more` comes from
  one extra row rather than a second `COUNT(*)`. Same validation as `/query`
  (one statement, SELECT/WITH only, no file readers).

## [0.13.0] — 2026-11-09

### Added

- **Background jobs** (`crates/core/src/jobs.rs`): a job registry with progress,
  phase, fraction, messages, output and error. A 10 GB split takes minutes, so
  the HTTP handler returns a job id and the client polls — the work is not tied
  to the connection.
- Write endpoints: `POST /split`, `POST /merge` (both 202 + job id),
  `GET /job?id=`, `GET /jobs`. Output paths must stay inside `--root`, same rule
  as reads. Finished jobs are retained (50) and running ones are never evicted.

### Fixed

- **Flaky integration test**: every test used `SystemTime::now().as_nanos()` for
  its temp directory, and macOS only resolves to microseconds — a dozen parallel
  tests could read the same value, share a directory, and delete each other's
  files mid-run. `roundtrip_plain` failed once this way. All helpers now mix in
  an atomic counter; the suite ran clean four times in a row afterwards.

## [0.12.0] — 2026-11-09

### Added

- **`sql` command + `/query` API**: DuckDB over the file, registered as view `t`.
  Handing GROUP BY / ORDER BY / window functions / type inference to a real
  engine instead of writing them by hand — this is the "depend on libraries
  rather than rebuild them" rule applied where it saves the most code.
- Safety, because this is reachable over HTTP: one statement only, `SELECT`/`WITH`
  only, DDL/DML/ATTACH/COPY/INSTALL/PRAGMA rejected, CSV/Parquet/JSON readers
  rejected (data may only come from the whitelisted file), results capped
  (default 1000, max 100k) with a `truncated` flag.
- DuckDB is statically bundled behind an optional `sql` feature; the CLI enables
  it, so no external install is needed.

### Measured

- 200k-row CSV, `GROUP BY city` with `count(*)` and `avg()`: **0.67 s** end to end
  including process start.

## [0.11.0] — 2026-11-09

### Added

- **`filter` command + `/filter` API**: `col=value`, `col~text`, `col>number`,
  `col empty` / `col notempty`. Scans sequentially (decoding GBK/UTF-16 on the
  way) and returns **row numbers**, so a client can jump to a hit with
  `preview --start-row` instead of loading the file.
- Numeric comparisons fall back to string order when a cell is not a number, so
  ISO dates and zero-padded ids sort correctly.
- `--json` on the CLI for scripting; `/filter` returns the same shape.

## [0.10.0] — 2026-11-09

### Added

- **`convert` command**: streaming transcode to UTF-8 (optionally with a BOM),
  `--from auto|utf-8|gbk|utf-16le|utf-16be`. Constant memory, so it works on the
  same files the rest of the tool handles. This is the escape hatch for UTF-16:
  convert once and the row index / random access work normally.
- `encoding::parse_name` so the CLI and future API can accept encoding names.

### Changed

- The index error for UTF-16 now says what to run instead of only what failed.

## [0.9.0] — 2026-11-09

### Added

- **Encoding detection and decoding**: UTF-8, UTF-8 with BOM, GBK (the usual
  "ANSI" export on Chinese Windows) and UTF-16 LE/BE (BOM or NUL pattern).
  Preview, column stats, splitting (including `--columns`), sorting and the HTTP
  API all decode before parsing, so Chinese headers are no longer mojibake.
- `/meta` reports the detected encoding.
- Row index refuses UTF-16 with an explicit message: its separators are two
  bytes, so byte offsets would silently misalign. UTF-8 and GBK are fine —
  their `,` and `\n` never appear inside a multi-byte character.

### Fixed

- `--columns` wrote its temporary list to a pid-derived file, so concurrent runs
  in one process (the test suite) overwrote each other's column names.

## [0.8.0] — 2026-11-09

### Added

- **`csv-splitter serve`**: a dependency-free local HTTP API for a separated
  front-end. Read-only endpoints `/health`, `/meta`, `/rows`, `/stats`, backed by
  the sparse index so a browser can request rows 4,300,000..4,300,040 of a 10 GB
  file in ~1 ms.
- Path safety: every request resolves inside `--root`, canonicalised, `../`
  rejected with 400. Binds loopback by default and warns otherwise. CORS enabled.
- 4 black-box tests that start the real binary on port 0 and speak HTTP to it.

### Changed

- Project direction: GUI is frozen; the roadmap now targets the engine, CLI and
  HTTP API. Front-end work is out of scope.

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
