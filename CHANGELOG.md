# Changelog

## [0.46.0] — 2026-11-10

### Added

- **Cross-table lookup** (`crates/core/src/lookup.rs`): `lookup orders.csv --key
  cust_no --from customers.csv --get name,city` — the VLOOKUP a spreadsheet user
  reaches for first.
  - The reference table is loaded, the main table is streamed: two sequential passes
    over bytes regardless of how large the main file is.
  - Misses are `#N/A`, not blanks (a blank cell reads as real data).
  - Duplicate reference keys keep the first row, like VLOOKUP; a column-name clash
    gets a `_ref` suffix instead of producing two identical headers.
  - `--max-keys` (default 5 M) refuses an oversized reference with the parameter to
    raise; `--from-key` covers a differently named reference key.

## [0.45.0] — 2026-11-10

### Added

- **Formula functions, widened to what a spreadsheet is used for**
  (`crates/core/src/formula/` now has `mod.rs` + `functions.rs`):
  - comparisons (`= <> > >= < <=`) and booleans rendered as `TRUE`/`FALSE`;
  - logic: `if`, `and`, `or`, `not`, `isblank`, `isnumber`, `iferror`;
  - text: `&` concatenation, string literals, `left`, `right`, `mid`, `trim`,
    `replace`, `concat`;
  - numeric: `floor`, `ceil`, `int`, `sqrt`, `mod`, plus the existing `round`/`abs`/`min`/`max`;
  - dates: `year`, `month`, `day` over ISO prefixes (`2024-03-15`, timestamps included).
- `if()` evaluates **only the branch it needs**, so the classic
  `if(qty=0, 0, amount/qty)` guard works instead of erroring on the branch it discards.

### Changed

- The formula module was split into a directory: parsing in `mod.rs`, built-in
  functions in `functions.rs`. Thirty functions and a parser in one file is how a
  module stops being readable.

## [0.44.0] — 2026-11-10

### Added

- **`/rows?where=|order=` now sees the journal.** A filtered view of an edited sheet
  used to answer with pre-edit bytes, because SQL reads the file. The edited sheet
  is materialised to a temp CSV first, cached per source (replaced when the journal
  changes) and refused above 2 GB with the instruction to `edit export` — better
  than quietly filling a disk.
- **Read-only CLI commands warn about pending edits** (`filter`, `sort`, `search`,
  `sql`, `analyze`, `stats`, `groupby`, `pivot`, `chart`, `preview`): one line naming
  the journal and the command that materialises it. Silent pre-edit answers were
  the biggest remaining source of "why does this row still say CDMX".
- Stale `csvs_sheet_*` temp copies from a killed server are swept at startup (older
  than an hour).

## [0.43.0] — 2026-11-10

### Added

- **Column metadata** (`crates/core/src/layout.rs`): display names, widths, frozen
  columns and number formats, stored in a `<file>.view` sidecar.
  - Deliberately separate from the journal: an edit changes what a cell *is* and
    belongs in the audit trail; a width changes how it *looks* and should be cheap
    to throw away. Merging them would turn "reset column widths" into an edit with
    an undo step.
  - Number formats: `#,##0.00`, `0.00`, `0`, `0%`, `0.0%`, `$#,##0.00`. A format
    promise applies to numbers only — `N/A` stays `N/A`.
  - `/sheet` returns the layout alongside the columns, so a client does not need a
    second request to render chrome; `GET /layout` reads and sets properties.

## [0.42.0] — 2026-11-10

### Added

- **Chart data** (`crates/core/src/chart.rs`): `chart --x month --y amount:sum --series
  city` returns the JSON every charting library already accepts — categories plus
  one array per series. Cost follows the number of categories, not the rows.
  - Missing category/series combinations are `null`, not `0`, so a line chart draws
    a gap instead of a fake drop to zero.
  - `--type line|bar|pie|area`, `--sort-by-value`, `--max-points` (default 5000,
    refused with the parameter to raise rather than allocating unbounded).
  - `GET /chart` serves the same shape.

## [0.41.0] — 2026-11-10

### Added

- **Conditional formatting** (`crates/core/src/format.rs`): rules are evaluated
  server-side and the answer is a list of cells to paint — `(row, column, style)`
  — never a restyled copy of the data, so a window costs one window read.
  - Rule language: `column>1000:bg=red`, `~`/`^`/`$` for text, `between lo,hi`,
    `matches` with a small regex subset (`\d`, `\w`, `[a-z]`, `{n}`, `{n,m}`,
    `.`, `*`, `+`, `?`, anchors), `empty`/`nonempty`, named or hex colours, `bold`/`italic`.
  - Later rules win for the same cell, so a general rule can be refined instead of
    every rule needing to exclude every other.
  - `format --summary` streams the whole file and counts matches per rule — the
    cheap way to notice a rule is too broad before anyone sees colour.
  - `GET /style` serves the same evaluation for a window, with the rules echoed back.

## [0.40.0] — 2026-11-10

### Added

- **Pivot tables** (`crates/core/src/pivot.rs`): `pivot --rows city --cols month
  --values "amount:sum"` streams the input once and writes a rows × columns
  matrix. Multiple measures, multiple row keys, `--order`, `--desc`, and
  `--qualify` (measure names in the header, needed as soon as there are two).
  - Memory follows the *matrix* (row keys × column keys × measures), not the input:
    1 M rows → 5 × 12 = 60 accumulators, 1.56 s in a debug build.
  - Two hard ceilings (`--max-columns`, `--max-cells`) fail with the parameter to
    raise, before memory is committed — pivoting on an id column is a mistake,
    not a swap storm.
  - `GET /pivot` serves the same matrix through the result cache.
- **`aggregate.rs`**: `AggOp` / `AggSpec` / `Accumulator` moved out of `groupby`
  so `groupby` and `pivot` share one implementation of compensated summation.
  Two copies would eventually print different totals for the same file.

### Fixed

- Test temp directories used `SystemTime::now()` nanosecond suffixes, which on
  macOS has microsecond resolution: parallel tests received the same directory,
  and one would delete the other's files mid-run. They now use a process-wide
  atomic counter.

## [0.39.0] — 2026-11-10

### Added

- **A `Sheet` abstraction** (`crates/core/src/sheet.rs`): one CSV plus its journal,
  with `columns()`, `rows()`, `window()`, `apply()`, `undo()`, `export()`,
  `formulas()` and `capabilities()`. Editing, viewing and exporting now go through
  the same type instead of each re-deriving the model.
- `GET /sheet` returns the sheet's columns, logical row count, operation list and
  a **capability list**, so a front-end can decide which controls to show without
  guessing at what the server supports.
- `/rows` applies the journal when the file has edits: the viewer shows the edited
  table, not the raw bytes, and the response reports `edited` + `operations`.
- `scripts/clean.sh` and `[profile.dev] debug = 0`: stress fixtures and build
  output had grown to ~83 GB on this machine. Fixtures are removed by the script;
  dropping debug info took `target/` from 72 GB to 3 GB for a full test run.

### Fixed

- **Renamed columns came out blank in windows** (the export path was fixed in
  0.38.0, the `/rows` path was still matching by the original header). Both paths
  now share `column_layout`, and a `Sheet` test covers it.

## [0.38.0] — 2026-11-10

### Added

- **Formula columns** (`crates/core/src/formula.rs`): `edit formula --name total
  --value "round(amount * qty, 2)"`. Expressions are parsed once and evaluated per
  row at read/export time — nothing is stored, so a computed column costs no disk
  and no extra memory however many rows the file has.
  - Grammar: `+ - * / %`, parentheses, unary minus, column references (bracketed
    form `[客户编号]` for names with spaces), `round/abs/min/max/len/upper/lower`.
  - Spreadsheet-faithful edges: empty cell counts as 0, non-numeric text and
    division by zero give `#VALUE!`, and a formula reading a column that the
    journal dropped fails **before** the export writes anything.
- Formatted output keeps six significant decimals, not float noise
  (`0.1 + 0.2` → `0.3`, `100 / 3` → `33.333333`).

### Fixed

- **Renaming a column blanked its data.** The export mapped output columns onto
  the *original* header, so a renamed column no longer matched and every value
  came out empty. Column layout is now resolved once, in one place
  (`workbook::column_layout`), and both export and windowed reads use it.

## [0.37.0] — 2026-11-10

### Added

- **Editing without rewriting: `edit` + a sidecar journal.** The base file stays
  immutable; every edit appends one CSV record to `<file>.journal`, and reading or
  exporting applies the journal on top. A 17 MB / 1 M row file with three edits
  gets a **60-byte journal**, and `edit export` materialises 999,998 rows in 2.2 s.
  - Operations: `set` (cell), `insert` (row), `delete` (rows), `replace` (find &
    replace in a column or everywhere), `drop` / `rename` (columns), `undo`,
    `status`, `export`.
  - Exposed as `POST /edit` (record an edit, undo, or start an export job) and
    `GET /edit` (pending operations and the logical row count).
  - **Edits are anchored to rows, not positions**: a `set` on row 500000 then an
    insert at row 7 keeps the edit on its original row instead of re-pointing it
    at whatever now sits in slot 500000. Cells are keyed by `RowId`
    (base physical row / insert-block id), resolved at journal-apply time.
  - Window reads (`workbook::window`) apply the journal to a slice via the sparse
    index, so the edited sheet is viewable without materialising it.

## [0.36.0] — 2026-11-10

### Changed

- **HTTP client errors are structured**: 400 now returns
  `{"error":…,"parameter":…,"hint":…}` so a front-end can highlight the offending
  input instead of showing prose, and a test can assert on `parameter` rather than
  on wording. Applied to path/out resolution, and to the required `by` / `agg` /
  `sql` / `q` / `id` parameters across every endpoint.
- The router's advertised endpoint list was stale: it omitted `/` and the newer
  `POST /sort`, `/dedupe`, `/partition`, `/warm`. A test now asserts the
  advertisement covers everything the docs mention.

### Docs

- `docs/ROADMAP.md` brought up to date: milestones closed or re-scoped, north-star
  numbers refreshed with the latest runs, and a new "known gaps" table that names
  what is *not* done (browser-verified UI, fast `analyze`, quote-style overrides,
  Windows, partial doc sync).

## [0.35.0] — 2026-11-10

### Added

- **`--verify` for `sort` and `partition`**, the two write paths that were still
  making unchecked claims. `sort` compares row and column counts before/after;
  `partition` unpacks the archive (including members that are themselves
  compressed) and sums the rows of every partition against the source.
  - This caught a real trap while being written: counting a `.csv.zst` member
    directly reported 577 rows out of 20,120, because the counter was reading
    binary noise. Members are now decompressed first.
- **Documentation drift tests** (`crates/cli/tests/docs.rs`): every
  `csv-splitter <command>` in a code block must be a real sub-command, every
  `GET/POST /x` must be advertised by the router, and the documented default port
  must actually serve the viewer.

## [0.34.0] — 2026-11-10

### Changed

- **One shape for every failure.** Usage errors are `error: <reason>` and exit 2;
  runtime failures are `✗ <command>: <reason>` and exit 1, optionally followed by
  a single `hint:` line. Previously the CLI printed five different forms, three
  of them from the background reporter, and two commands could print the same
  error twice.
- `hint:` appears only when there is a concrete next step — unsupported encoding
  points at `convert`, an unknown column at `analyze`, too many groups at
  `--max-groups` — because a hint with nothing to do is noise.
- A CLI test pins the contract: prefixes, exit codes, and that an error is
  printed exactly once.

## [0.33.0] — 2026-11-10

### Added

- **A contract test for the built-in viewer** (`crates/cli/tests/viewer.rs`): it
  extracts every `api("/x")` call from the HTML, checks each endpoint against the
  list the router advertises, then replays the page's request sequence against a
  live server and asserts the exact fields the JavaScript reads. A browser test
  could not run in CI; API drift is what actually breaks the page.
- Every `/rows` response now carries `columns`, including the plain paged path.

### Fixed

- The plain `/rows` window returned `rows` without `columns`, so the viewer's
  table had no header to render — found immediately by the new contract test.

## [0.32.0] — 2026-11-10

### Added

- **A built-in viewer at `GET /`**: one self-contained HTML file (no build step,
  no dependencies, ~13 KB) with a file list, row paging, jumping to a row, filtering,
  cross-column search, the column profile, and group-by. It calls only the public
  endpoints, so it doubles as a reference implementation for a real front-end.
- **`/rows` understands `where` and `order`** (plus `desc`), served through the
  same cached SQL path as `/rows?sql=` so the two cannot disagree.

### Fixed

- **DuckDB sniffing failures did not fall through the reader ladder.** Errors
  raised while creating the view (where sniffing happens) aborted immediately
  instead of trying the next option set, so a file with ragged rows *and* quoted
  newlines could not be queried at all. `null_padding` + serial is now reachable.

## [0.31.0] — 2026-11-10

### Added

- **`csv-splitter demo`**: writes a realistic sample file (20,120 rows, 918 KB)
  with the defects real exports have — duplicate rows, empty cells, a quoted
  field containing a newline, and rows with an extra unquoted comma. Deterministic,
  so the documentation can quote its output and keep it true.
- **[docs/GETTING_STARTED.md](docs/GETTING_STARTED.md)**: a ten-step tour that
  starts at `demo` and ends at `serve`, with the real output of every step, plus
  a one-page command table. The README now opens with a quick start that points
  at it.
- A CLI test runs the *documented* tour end to end, so the guide cannot silently
  drift away from the program.

## [0.30.0] — 2026-11-10

### Field run on a 1 GB file

Ran the whole pipeline on 1.0 GB / 19 M rows with `--verify` on every write:

| step | time | peak RSS |
|---|---|---|
| `preview` open + jump to row 18,000,000 | 0.01 s + 0.56 s | — |
| `analyze` all columns | 10.9 s | **29 MB** |
| `search` across every column | 8.1 s | — |
| `filter -o --verify` (2,370,440 rows) | 5.6 s | — |
| `dedupe --by --verify` | 5.2 s | — |
| `groupby` (3 aggregates) | 6.3 s | — |
| `sort` two keys | 25.8 s | 227 MB |
| `columns --verify` (19 M rows × 5 cols) | 11.9 s | — |
| `partition --by` (zst) | 19.8 s | — |
| `split -n 8 --threads 4` / `merge` | 1.1 s / 0.69 s | 121 MB |
| `verify` byte-for-byte | 5.2 s | — |

Every step finished under 30 s, none needed the file in memory, and every
self-check passed on the first try.

### Fixed

- **Large sums printed float noise**: a 2e11 total came out as
  `215064677256.200012`. f64 has ~15 significant digits, so summing 19 M values
  leaves error in the last few bits — printing it down to the micro-unit was
  pretending to precision that is not there. Values ≥ 1e9 now keep 2 decimals.

## [0.29.0] — 2026-11-10

### Added

- **`--verify` on the write commands** (`filter`, `columns`, `dedupe`): re-reads
  the output and checks the claim the operation makes — exact row count, exact
  column count, and no raggedness beyond what the source already had.
  `dedupe` additionally checks `kept + removed == scanned` before reading
  anything. `split`/`merge` already had the byte-for-byte `verify`.
- `dedupe` reports `kept_ragged` and `filter` reports `matched_ragged`, so the
  check can distinguish "this operation wrote bad rows" from "the source was
  already ragged and the rows were copied faithfully".

## [0.28.0] — 2026-11-10

### Fixed — extreme inputs

- **Windows-1252 was silently decoded as GBK**, turning `café` into mush. Two
  candidates are now scored: GBK wins only when it decodes cleanly *and* yields
  CJK characters; otherwise Windows-1252 (which never fails to decode) is used.
  Files with real Chinese text still detect as GBK, and plain UTF-8 is untouched.
- **`split` refused a file with no data rows** ("No data lines"). A 0-byte or
  header-only export is a legitimate thing to process, so it now produces one
  chunk holding the header and merges back **byte for byte**.
- **`analyze` errored on a 0-byte file**; it now reports that there are no
  columns instead of failing.
- `/meta` reported a phantom single empty column for a 0-byte file; it reports
  `headers: []`.

Verified against fixtures for: 0 bytes, header-only, blank lines only, no trailing
newline, duplicate header names, NUL bytes inside a field, a 2 MB single-line
field, UTF-8 BOM, UTF-16 (explicitly refused with a message that says what to do),
and Windows-1252.

## [0.27.0] — 2026-11-10

### Fixed — found by running a deliberately dirty file end to end

A GBK file with CRLF endings, stray spaces in the header (`城市 `), ragged rows
and quoted newlines exposed four things:

- **`partition` could not read a GBK file at all.** It parsed the sorted chunk's
  header as UTF-8, so the partition column looked missing and the error named
  mojibake candidates. It now decodes while reading.
- **Reported errors were swallowed.** `spawn_reporter` folded the engine's error
  into the reporter's return value, and every caller discarded it — commands
  failed with "see messages above" and nothing above. The reporter now prints the
  reason itself.
- **Column names with stray spaces were unusable.** `--where "城市=CDMX"` could
  never match a header literally named `城市 `. Names now resolve exact →
  unique-trimmed → unique-case-insensitive, and an ambiguous match is refused
  rather than guessed. A missing name reports the available columns, or the
  closest one when there is a plausible spelling difference.
- **Ragged rows were invisible.** `analyze` now reports how many rows have a
  field count different from the header, which is the first thing worth knowing
  about a foreign file.

Verified: record counts match Python's `csv` module exactly on the dirty fixture,
and `verify` still round-trips the file byte-for-byte.

## [0.26.0] — 2026-11-10

### Added

- **`groupby`**: `--by city --agg "amount:sum,amount:mean,amount:count"`,
  multi-key and multi-aggregate, `--order` an output column, `--desc`. Memory
  scales with the number of **groups**, not rows; past `--max-groups` (1M) it
  errors instead of eating the machine, because grouping by an id column is
  almost always a mistake. Also `GET /groupby` (served from the result cache).
- Summation is Kahan-compensated: adding 40k values with plain f64 showed
  visible drift in the last printed digits.
- A text column aggregated with `sum`/`mean` renders `<mixed>` rather than
  silently averaging only the numeric rows.

## [0.25.0] — 2026-11-10

### Added

- **`partition --by COLUMN`**: one file per distinct value, packed into a tar/zip.
  The engine already existed; it is now a command and `POST /partition`. It sorts
  by the column first so only one writer is open at a time, and refuses to exceed
  `--max-partitions` (512) because partitioning by `id` is almost always an
  accident.
- **`concat a.csv b.csv ...`**: glue same-shaped files into one table.
  Headers are **checked**, not assumed: a mismatch is an error naming the file and
  its columns, because silently appending amounts under an id column corrupts the
  table invisibly. `--ignore-header` opts into that on purpose and still reports
  which files differed.

## [0.24.0] — 2026-11-10

### Added

- **`columns`**: reshape before handing data to someone else — `--keep a,b`
  (also reorders), `--drop a,b`, `--rename old:new`. Unlike `filter --out`, this
  must re-serialise each row because the column set changed, so quoting is
  applied consistently and a multi-line cell stays one field. A column that is
  both kept and dropped is an explicit error rather than a silent winner.
- **`in (a,b,c)` filter**: `--where "city in CDMX,Puebla"` — one condition
  instead of a chain of ORs. Values are trimmed and matched case-insensitively;
  rendered as a quoted `IN (...)` list for `GET /export?where=`.

## [0.23.0] — 2026-11-10

### Added

- **Multi-column sort**: `sort --by "city:asc,amount:desc"`. Each key carries its
  own direction; `--desc` only sets the default for keys without one. The
  comparison rule is resolved once into a shared plan, so the chunk writer and
  the merge heap cannot disagree — a mismatch there would produce a "sorted" file
  that is unsorted at chunk boundaries.
- **Regex filtering**: `col~re:PATTERN` uses a real regex engine (`regex` crate)
  while `col~TEXT` stays a plain substring search. Patterns are validated at
  parse time and compiled **once per scan**, not per row. Available through
  `filter --where`, and translated to `regexp_matches()` for `GET /export?where=`.

## [0.22.0] — 2026-11-10

### Added

- **`analyze`** (`crates/core/src/profile.rs`): a per-column profile in one
  streaming pass — empty counts, distinct counts (exact up to a cap, then `≥N`),
  numeric min/max/mean/sum/median, and for text columns the common values plus
  length extremes. A column counts as numeric only when *every* non-empty value
  parses; one `N/A` demotes it to text and is reported as `mixed`. Also
  `GET /analyze`.
- **`dedupe`** (`crates/core/src/dedupe.rs`): drop duplicate rows by the whole
  record or by chosen columns, keeping the first occurrence. Only 128-bit hashes
  are retained (16 bytes per distinct row), and passing `--max-keys` fails loudly
  instead of silently dropping unique rows. Also `POST /dedupe` as a job.

## [0.21.0] — 2026-11-09

### Added

- **Multi-condition filtering**: repeat `--where` for AND, add `--any` for OR.
  The common "city=CDMX and amount>1000" no longer needs a trip to SQL.
- **`filter --out FILE`**: stream the matches into a CSV instead of only listing
  row numbers. Matched rows keep their source bytes — no re-serialisation, so
  quoting and line endings stay exactly as they were.
- **`sort` command**: whole-file external sort exposed on its own
  (`sort in.csv --by amount --desc -o sorted.csv`); previously it was only
  reachable as a `split` option.
- **API**: `GET /export?where=a=1|b>2` returns the filtered CSV, and `POST /sort`
  runs as a job like split/merge. Predicates are parsed with the CLI grammar and
  rendered as SQL, so a value can never be read as a column name or break out of
  its literal.

### Fixed

- A `/sort` job stayed `running` forever after the file was already complete:
  it never sent the `Finished` event the job registry waits for.

## [0.20.1] — 2026-11-09

### Added

- **`GET /download?path=REL`**: streams the file byte-for-byte with
  `Content-Disposition: attachment`. Root confinement applies like every other
  endpoint, and the response is raw bytes — no UTF-8 round trip — so a GBK file
  or a file with a BOM comes out exactly as it went in (tested with non-UTF-8
  bytes in the fixture).
- **`GET /export?path=REL&sql=...`**: the query result as CSV text instead of
  JSON, properly quoted (`a,b` becomes `"a,b"`), so the output can be piped into
  another tool or saved as a file.

## [0.20.0] — 2026-11-09

### Added

- **`search`** (`crates/core/src/search.rs`): case-insensitive substring across
  every column (or `--columns a,b`), `--case`, `--exact`, `--limit`, `--show`.
  Returns row numbers, so hits can be jumped to through the sparse index.
  Exposed as `GET /search?q=...`. Matches inside quoted multi-line fields count
  once and do not shift later row numbers.
- **`files`** (`crates/core/src/listing.rs`): list the tree under `--root` by
  extension, relative paths, size and mtime — what a front-end needs to render
  a file picker. Exposed as `GET /files?dir=...&ext=...`; unreadable
  subdirectories are skipped and symlinked directories are not descended.

### Changed

- README now opens with a "what do I actually want to do" table mapping each basic
  task to its command and its HTTP endpoint.

## [0.19.0] — 2026-11-09

### Added

- **Parallel splitting** (`--threads N`, `threads=` on `POST /split`): several
  readers, each owning a contiguous run of chunks. 1 GB / 8 chunks went from
  2.8 s to **1.0 s** with 4 threads (zstd: 3.5 s → 1.5 s); 8 threads add nothing
  on an M1 Pro.
  - The hard part is not parallelism but *where to start*: seeking to
    `file_len/n` lands mid-record, possibly inside a quoted field containing a
    newline. The counting pass now records checkpoints (byte offset every 1000
    records, known outside quotes), readers start from one and each owns whole
    chunks, so no record is split, duplicated or reordered.
  - Verified by a test that splits the same file with 1/2/4/8 threads and
    requires byte-identical output after merging.

### Fixed

- The performance guard was flaky when the whole workspace ran in parallel: all
  three tests built their own 10 MB fixture while measuring throughput. One
  shared fixture plus best-of-three measurement removed the noise (three
  consecutive full runs clean).

## [0.18.0] — 2026-11-09

### Added

- **`POST /warm`**: pre-load a query into the result cache as a background job.
  Files with quoted newlines fall back to a serial CSV scan, so the first page of
  a 10 GB query costs seconds — a client that knows its filter can warm it up
  while the user is still reading the empty table. Warmed page: **0.03 s**
  instead of 0.70 s.
  - `200 {"cached":true}` when already warm, so polling clients do not spawn
    duplicate jobs; invalid SQL is rejected with 400 before a job exists.

## [0.17.0] — 2026-11-09

### Added

- **Query result cache** (`crates/core/src/cache.rs`): `/rows?sql=` materialises
  the result once (200k rows per entry, 400k rows total) and later pages are
  slices. On a 1 GB file with quoted newlines, page 1 costs 0.70 s and every
  following page costs **0.00 s**.
  - A result bigger than the cap caches only its prefix; `rowsSoFar` says how
    much, requests inside the prefix are served, and a request past it returns
    422 instead of pretending the query ended there.
  - Entries are invalidated by file length or mtime, evicted LRU, and partial
    results never masquerade as complete.

### Fixed

- **DuckDB could not read files with quoted newlines.** `null_padding` (needed
  for ragged rows) is rejected by the parallel scanner when a field contains a
  newline, so any CSV with embedded newlines failed with a raw engine error.
  Opening now tries three option sets — parallel+padding, parallel, serial+padding
  — and only retries on reader-shape errors, so a genuine query error is still
  reported once.
- The wrong-delimiter hint moved from the query result to `DESCRIBE t`; it used
  to fire on `count(*)` (a single column by design) instead of on the table.

## [0.16.0] — 2026-11-09

### Added

- **criterion benchmarks** (`crates/core/benches/throughput.rs`): index build,
  random access at head/middle/tail, and split throughput at 1 and 4 chunks.
  Informational only — shared runners are too noisy for thresholds.
- **Performance guard** (`crates/core/tests/performance.rs`, runs in 0.2 s):
  fails the build only on pathological regressions (~50x slower than measured),
  so it catches an accidental O(n²) or a lost buffer without flaking on CI.

### Changed

- **CI split into two jobs.** DuckDB is statically bundled and takes ~10 minutes
  to compile from cold, which used to delay feedback on the engine — the part
  that changes most. `engine` (core + gui, no DuckDB, includes fmt/clippy/perf
  guard) now runs independently and finishes in minutes; `cli` (DuckDB, HTTP API
  tests, benchmark compile check) follows it.

## [0.15.0] — 2026-11-09

### Verified at 10 GB

Measured on a 9.9 GB / 190 M row CSV (Apple M1 Pro, release build):

| Operation | Time | Peak RSS |
|---|---|---|
| `index` | 4.28 s | 18.3 MB |
| jump to row 189,000,000 | 0.5–1.2 ms | 11.3 MB |
| `split -n 8` | 20.9 s (≈490 MB/s) | 89 MB |
| `merge` | 6.1 s | 10.7 MB |
| SQL group-by (5 groups) | 2.78 s | — |
| `/rows?sql=…&start=1000000` | 4.25 s (full sort) | — |

Index size is 1.45 MB for 9.9 GB — 1/7000 of the file — because it stores one
offset per 1000 rows.

### Fixed

- **DuckDB refused ragged CSVs.** A stray unquoted comma (very common in real
  exports) made `read_csv_auto` bind the header as one giant column, and every
  query then failed with "column not found". Our own reader tolerates such rows,
  so `/rows` and `/rows?sql=` disagreed about the same file. Now opened with
  `header=true, delim=<requested>, null_padding=true`, and a single-column
  header that still contains a separator produces an explicit message naming the
  likely delimiter instead of a binder error.
- `/query` and `/rows?sql=` now honour the request's delimiter.

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
