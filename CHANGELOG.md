# Changelog

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
