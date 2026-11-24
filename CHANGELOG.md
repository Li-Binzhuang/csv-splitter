# Changelog

## [4.2.0] — 2026-11-20

### Added

- **Whole-column aggregates in formula columns**: `sum(col)`, `average(col)`,
  `count(col)` and the conditional `countif(col, "criteria")` / `sumif(col, "criteria")`
  are constant for every row, so one pre-pass computes the column's numeric
  total and count and the row loop reads them in O(1) — the same pattern `lag` and
  `running_total` already use. Non-numeric and empty cells are skipped, like a spreadsheet.
  Measured on 1 M rows: `edit export` 0.85 s without the formula, 1.33 s with it, so the
  pre-pass costs about 0.48 s. The criteria strings are Excel-shaped (`">1000"`,
  `"<>10"`, a bare `CDMX` for equality) and are compiled through the same parser
  `filter --where` uses; ordering criteria only count numeric cells. With the columnar sidecar (`csvx cache FILE --build`,
  0.99 s for this 33 MB file) the pre-pass reads parquet through DuckDB and drops to
  **0.30 s** (export 1.15 s); DuckDB's start-up is most of what is left, so the 50 ms
  target is still open.

### Known limits

- The pre-pass reads the **base file**; pending journal edits are not included (the
  export the formula runs in is also the command that materialises edits).
- `sum(A1:A9)` (range references) are not implemented; criteria are a single comparison,
  not a wildcard pattern.
- The 50 ms target in `docs/BEYOND_EXCEL.md` is now met for a repeat export: the totals
  are cached in `t.csv.agg` (invalidated by length + mtime), so the pre-pass on 1 M rows
  drops to ~30 ms; the first, cold call still costs about 0.09 s.
- A formula column used to force a full re-render of every row: 2.1 GB / 50 M rows took
  ~70 s. A formula that is the same on every row (whole-column aggregates) now rides the
  byte-copy path by appending one cell: the same export takes 7.2 s (1 M rows: 0.13–0.33 s); the pre-pass itself splits
  at checkpoints when the file is large, which took 15.4 s -> 7.2 s, and only formulas that read the current row still rewrite each row.

### Fixed
- A transaction is undone (and redone) in one step: the batch boundary is recorded next to
  the journal (`.journal.blocks`, and `.journal.redo.blocks` after an undo), so both
  `edit undo`/`redo` and `POST /edit?action=undo|redo` move the whole batch while a plain
  single edit still moves one operation. Tests cover both
  surfaces and that a marker never swallows earlier single edits.

- An insert anchored past a delete was dropped by `edit export`: with an unknown base
  length the row map kept a phantom base slot before the inserted row, and the export's
  "physical rows exhausted" break stopped first. Export now builds the row map with the
  real row count when inserts exist (deletes alone still skip the index). A test pins
  the delete-then-insert export.

## [4.1.0] — 2026-11-20

### Performance

- **A whole-table filter or search is now several scans at once.** UTF-8 files are split at
  the sparse index's checkpoints into one slab per core (capped at 8); each slab seeks,
  parses and matches on its own, and the row numbers are merged in order. GBK/UTF-16 keep
  the sequential path because a byte offset there is not a row boundary.
- Measured on 2.1 GB / 50 M rows (release, M-series): `filter city=Tijuana` (12.5 M hits)
  **13.5 s → 2.7 s**, `search` with no hit **18.1 s → 4.8 s**, peak RSS 42–43 MB. The HTTP
  endpoints inherit the win (`/filter` 2.1 s, `/search` 3.1 s, `/rows` 6 ms on the same
  file), so a huge CSV no longer trips the 30 s budget on the way to a 504.
- Row numbering is unchanged: `parallel_scan_agrees_with_sequential` and search's twin
  compare the parallel path against the sequential one record by record, including a
  quoted newline that crosses a checkpoint. `CSVX_SCAN_THREADS` overrides the thread count
  (1 keeps the sequential path in the same build), which is how the before/after numbers
  above were taken. At 11.2 GB / 250 M rows the drill gives filter **11.9 s**, search
  **14.8 s** and HTTP 10–16 / 14–20 s (load-dependent) — inside the 30 s budget, with
  ~20–22 GB as the point where search would need cursor pagination instead.

### Added

- **`averageif`**: `averageif(amount, ">5")` and `averageif(B2:C2, ">5")` reuse the
  same matching (sum, count) as `sumif`, so no extra scan; no matching cell is `#VALUE!`
  instead of a division by zero.
- **`mode(range)`**: the most frequent numeric value of a same-row range (MODE.SNGL);
  ties pick the smallest value so the answer is deterministic, and no repeated value is
  `#VALUE!` rather than a made-up first element.
- **`percentile(range, p)`**: the p-quantile (0..1) of a same-row range with
  PERCENTILE.INC interpolation; text and blanks are ignored, p outside 0..1 is
  `#VALUE!`.
- **`sumsq` / `product`**: one or more same-row ranges, skipping text and blanks;
  product of no numeric cell is `#VALUE!` rather than an invented 1.
- **`large(range, k)` / `small(range, k)`**: the k-th largest/smallest value of a
  same-row range (k is 1-based and truncated toward zero); text and blanks are ignored,
  and k outside 1..n is `#VALUE!`.
- **`median(range)`**: returns the median of a same-row range, ignoring text and blanks;
  a whole-column median is `#VALUE!` because it would have to keep every value.
- **Aggregate pre-pass**: whole-column sums no longer build a field vector per record,
  and the column-name key is no longer reallocated for every numeric cell. On a 1M-row
  file the cold pre-pass measures ~25-30 ms (2026-11-23 recheck: cold export 0.11 s minus
  warm 0.08 s, 8 threads; the earlier run read 0.12/0.09). The same 1M rows with
  `countif` and with `countifs` also read ~30 ms (0.09-0.10 and 0.11 cold against
  0.06-0.08 warm), so filtered and set paths are not a hidden cost;
  a run immediately after a rebuild can still read ~0.6 s while binary and file pages are
  cold, so compare medians, not single runs.
- **Analysis equivalence coverage**: the native-vs-DuckDB byte-for-byte tests for
  pivot now compare count/sum/mean/min/max and a second measure column, and the groupby
  test compares min/max as well, so the documented op set is actually exercised.
- **`stdevp` / `varp`**: population standard deviation/variance (Excel STDEV.P/VAR.P)
  from the same sum-of-squares pass; `varp(B2:C2)` works over a same-row range too.
- **`min`/`max`/`stdev`/`var`**: `min(amount)` / `max(amount)` read numeric bounds,
  `stdev(amount)` / `var(amount)` the sample deviation/variance (Excel STDEV.S/VAR.S),
  all from the same pre-pass as the sums; `min(B2:C2)` etc. do the same over a same-row
  range. The `.agg` cache gained `bounds` and `squares` lines and was bumped to version
  3, so an old cache is recomputed instead of misread.
- **`countifs` / `sumifs`**: `countifs(city, "CDMX", qty, "<5")` and
  `sumifs(amount, city, "CDMX", qty, "<5")` evaluate every criterion in the same
  pre-pass and report how many rows match all of them (and their sum). The cache gained
  a `set` line and moved to version 4; a range argument is `#VALUE!` rather than
  silently counting this row.
- **`countblank`**: `countblank(amount)` and `countblank(B2:C2)` count empty cells.
  The pre-pass already skipped them for sums, so it now also keeps their count; the
  `.agg` cache gained a `blank` line and moved to version 5.
- **Scaling note**: a 2026-11-23 recheck on 11 GB at 280M rows (12% more rows than the
  delivery drill) reads 14.9 s filter / 16.3 s search against 13.2–13.9 / 13.8–14.6 at
  250M rows, with the same 78–81 MB. Filter/search cost tracks row count, not bytes;
  CAPABILITIES tells readers to estimate capacity from rows.
- **Multi-table references**: one formula row can read several reference sheets at
  once (the CLI test exports customer and region from two files in one pass); the
  gap table now states that `Sheet!A1` is not planned because row-aligned random
  access across files is not available to a streaming export.
- **`workday(start, days)`**: moves a date by business days, skipping weekends and
  accepting negative days (no holiday list in the two-argument form); zero keeps the
  date.
- **81 GB / 1.16B-row 7-column drill**: search 39.42 s, filter 46.70 s, full 17%
  export 196.9M rows/13 GB in 62.03 s at 28 MB RSS (still a repeated 21.8 GB block).
- **100 GB full export**: two-pass counting + direct range writes remove the part-file
  double write; 200M-row/20 GB export drops to 79.62 s at 28 MB RSS (from 98.56 s),
  now under 90 s. Adaptive at 64 GB keeps the faster part path for smaller files
  (29 GB A/B: selective 7.21 vs 14.52 s, 20% 15.18 vs 17.90 s). The single-writer
  experiment was reverted as a serialisation regression.
- **100 GB profile**: 104.5 GB/1.3B rows search 20.18–20.42 s (93 MB), filter
  25.75 s (95 MB), selective filter -o 19.54 s, index 24.57 s, deep view 0.02 s;
  meets <60/90 s and <200 MB targets. See docs/BENCHMARKS.md.
- **MCP async jobs**: `csvx_job_submit` + `csvx_job_status` run tools in a child
  process (no server blocking), cache the finished output, and report exit code/error.
- **MCP 2.3 GB drill**: cold schema 3.11 s (760 KB index), warm 0.03 s, deep view
  0.02 s, filter 0.77 s, search 0.56 s, quality 22.25 s; responses stay 0.4–4 KB.
- **Official MCP SDK check**: `@modelcontextprotocol/sdk` 1.32.2 Client over
  StdioClientTransport lists 13 tools and completes schema/filter/view/edit/undo/convert/job.
- **End-to-end demo**: `scripts/demo_end_to_end.sh` + dependency-free
  `scripts/mcp_client.py`; pipeline 12/11 ms, 13 MCP tools, edit/undo/convert/job done.
- **MCP job retention**: completed job history is capped at 128 entries; pruning removes
  the oldest completed entries and their temp stdout/stderr files. Server shutdown cancels
  running jobs and removes all job output files.
- **MCP write tools**: `csvx_edit_set/delete/undo/redo` and `csvx_convert` share the
  CLI journal lock and root confinement; CLI MCP test covers edit→view→undo and TSV export.
- **`mcp`**: stdio JSON-RPC MCP server with read-only tools
  (`csvx_schema/view/filter/search/quality/pipeline_status`), root-confined paths,
  UTF-8 arguments; initialize/tools/list/tools/call covered by a CLI test.
- **Packaging & pilot confirmation**: `scripts/package_release.sh` builds an
  offline tarball with SHA256 (no tag, no registry), self-verifies it with
  `verify_dist.sh`, and deletes the tarball/sha if verification fails; PILOT_CONFIRMATION.md is a fill-in 4-week pilot agreement template.
- **View cursor**: `view --json` (and the MCP `csvx_view` tool) now returns
  `start`, `nextStart` and `hasMore`; clients can page without guessing counts.
- **MCP cache stats**: `csvx_cache_stats` (readOnlyHint) exposes hits/misses,
  entries and bytes; `csvx_cache_clear` (destructiveHint/idempotentHint) deletes cache and dead-process
  temp files, returning removed/stale counts and bytes.
  Neither is a CLI job command.
- **Operations contract**: `csvx ops --json`, `GET /ops` and MCP `csvx://operations` publish
  the same 14 operations, four effects and 10 error codes; ADR in `docs/OPERATIONS.md`.
- **Typed read operations**: `core::ops` owns schema/view/filter requests and responses;
  `schema` and `filter` CLI/HTTP now use typed requests; view migrates next.

- **Pipeline watch**: `csvx pipeline watch <plan>` reruns stale steps on an interval;
  supports failure backoff, `--max-failures`, JSON rounds, and an atomic `--state` file
  with last success/failure timestamps, next retry delay, the last error text, and
  round resume across restarts.
- **MCP pipeline DAG options**: `csvx_pipeline_run` accepts dag/jobs/clean and forwards
  them to the CLI, so agents can run topological waves and bound concurrency.
- **MCP structured results**: every tool response carries
  `structuredContent: {"ok":bool,"code":"ok|tool_error|unknown_tool|unknown_job|path_escape|cli_error"}`
  next to text content; tests cover unknown_tool and path_escape.
- **Content-check fast-skip guard**: `--content-check` hashes input/output even when
  size+mtime match, also for `pipeline status --verify`; files ≤1 MiB hash by default,
  larger ones require the flag; `CSVX_CONTENT_CHECK_LIMIT` tunes or disables the auto limit. Tests cover small default, large flag, and status.
- **Stable step markers**: `--progress-markers` emits
  `CSVX_STEP done|skip <step> <i>/<n>` **as each step completes** (not after the run);
  MCP streaming parses it and includes `total` in progress notifications. Test asserts the marker and total form.
- **Pipeline step progress**: `csvx_pipeline_run` with a progressToken streams the
  CLI's step lines and emits one notification per step; without a token it keeps
  returning the JSON result.
- **MCP progress notifications**: when tools/call carries `_meta.progressToken`,
  job_status wait emits `notifications/progress` (0 then once per second). Test
  asserts the notification line and token.
- **MCP job list summary**: default list entries are `state/outputChars` rather than
  full payloads; `full=true` restores the previous behavior.
- **MCP job wait**: `csvx_job_status` accepts `wait` (0–30 s) and long-polls the
  child instead of returning running immediately; test asks for a 2 s wait on a
  quick schema job and gets done.
- **MCP job list**: `csvx_job_list` returns every job's state; running jobs also
  report `elapsedMs`. 18 tools (including cache stats/clear); the schema unit test guards tools/list JSON.
- **MCP job cancel**: `csvx_job_cancel` kills a running child and returns
  `{"state":"cancelled"}`; finished jobs return done/failed. Reader threads and
  cached results are cleaned up.
- **MCP pipeline_run**: agents can execute a declarative pipeline (force/dryRun)
  through MCP; long runs can be wrapped in the existing job tools.
- **Compressed input**: search/filter/analyze read .zst/.gz/.bz2/.xz through a
  decompressing reader; non-seekable forces the sequential path. 2.3 GB zst filter
  writes a byte-identical 411 MB output in 4.16 s at 15 MB RSS (plain 1.51 s).
- **Cache stats**: setting `CSVX_MCP_CACHE_STATS` makes the MCP server print
  materialize-cache hits/misses on exit.
- **Cache env diagnostics**: invalid `CSVX_MCP_CACHE_BYTES/ENTRIES` values warn once
  and fall back to the defaults instead of silently changing behavior.
- **MCP materialize cache**: the server decompresses a compressed input once and
  reuses the temp CSV for later search/filter calls keyed by path+size+mtime+ctime+inode;
  cache is capped at 4 GiB/16 entries (tunable via CSVX_MCP_CACHE_BYTES/ENTRIES) with
  LRU eviction; startup removes dead-PID temp files; repeated queries measured
  gzip 2.04 GB 4.61→1.63 s/86 MB, zst 0.279→0.101 s, bz2 0.693→0.100 s.
- **MCP materialize options**: `csvx_search`/`csvx_filter` accept materialize and
  forward `--materialize`, so agents can search compressed inputs with random access.
- **Search/filter materialize**: `--materialize` streams compressed input to a temp
  file and searches/filters it with random access; 2.3 GB zst search went 11.87 s ->
  3.51 s with the same 19M matches; filter scan went 6.94->3.62 s and export
  4.06->2.60 s with byte-identical output. Temp file is removed on exit; a later
  materialize also removes leftovers whose PID is dead.
- **Staging cleanup**: each run removes matching `.branch-*.tmp`/`.pipeline.tmp`
  leftovers older than an hour; `--clean` forces cleanup before executing. Test uses
  dry-run so the cleanup can be verified without touching outputs.
- **Topological DAG**: `--dag --jobs N` schedules ready steps from the lineage
  graph (empty input → previous step, step name → earlier step, file → no step
  dependency), freshness-checks each ready step and runs stale ready steps in
  bounded concurrent waves. Test covers a diamond and all-fresh second pass.
- **Live locks are never age-stale**: the 24h fallback now applies only when the
  lock has no readable PID; a live owner is kept even if the lock is older. Test
  sets a 2-day-old lock with the current PID and asserts refusal.
- **Dead-owner lock cleanup**: the lock records its PID and `ps -p` proves whether
  it is alive; a dead owner is removed immediately instead of waiting 24h. Test
  plants a fake PID and asserts the run proceeds.
- **Unlock escape**: `pipeline run --unlock` removes the plan lock first; test
  holds a false lock and asserts the run succeeds and the lock is released.
- **Run lock**: `<plan>.pipeline.lock` blocks a second run on the same plan; locks
  older than 24h are treated as stale and removed. Test holds a lock manually, asserts
  refusal/no output, then runs and asserts release.
- **Fail-on-stale**: `status --verify --fail-on-stale` exits 3 on any stale row,
  so cron/CI can gate on freshness; test covers the stale exit.
- **MCP status verify**: `csvx_pipeline_status` accepts verify and returns fresh
  fields; test runs a DAG, then verifies the stored state over MCP.
- **Canonical plan paths**: existing paths are canonicalized before collision
  checks, so a symlink alias of the source is rejected as in-place output.
- **Lexical path normalization**: duplicate-output and in-place guards compare
  normalized paths, so `./a.csv` cannot bypass `a.csv` checks; tests use `./` forms.
- **In-place source guard**: a step output equal to any declared source input is
  rejected, so `filter ... -o orders.csv` cannot silently replace the original.
- **Duplicate output validation**: a plan whose two steps write the same output is
  rejected before locking/executing; test asserts no file is created.
- **Status verify**: `pipeline status --verify` recomputes stamps/hashes against
  current files and reports fresh/stale; test corrupts an output and sees stale.
- **Persisted lineage**: successful linear/DAG runs write `<plan>.lineage.csv`
  (`step,deps`) next to the plan; dry-run and failures do not write it.
- **Pipeline lineage**: dry-run JSON now includes each step's `deps` (previous
  step for empty input, earlier step names, or none for source files); text dry-run
  prints the same edges. No execution/state write.
- **Pipeline groups**: optional 6th manifest column `group`; consecutive steps with
  the same group run as concurrent child pipelines under `--jobs N`. Group members
  must name an explicit input and cannot depend on each other. Each member is
  freshness-checked; fresh members reuse their prior row while only stale members
  are spawned concurrently. Children write staging files; only a fully successful
  wave renames them over the final outputs. A failure kills siblings and removes
  staging, leaving the previous outputs untouched and the main state unwritten.
  DAG mode backs up old outputs before each wave and restores all of them if a
  later wave fails; a later run also auto-recovers backups left by a crash.
- **Pipeline retries**: `--retries N` / `--retry-delay MS` retry failed or timed-out
  steps; each attempt starts from a removed temp output, final failure stays atomic.
- **Pipeline step timeout**: `--step-timeout <ms>` kills a hung child, removes the
  temp output and leaves `.pipeline.csv` untouched; reader threads drain output so
  a verbose step cannot deadlock the parent.
- **Pipeline --dry-run**: reports fresh vs would-run steps without executing or
  writing `.pipeline.csv`; `--json` now emits `dryRun/steps` so MCP callers get
  structured output. Covered by the CLI pipeline test.
- **Pipeline fast skip**: state records input/output size+mtime; an unchanged 2.3 GB
  pipeline reruns in 17 ms (was 5.08 s of hashing). Touching the same bytes still
  invalidates via mtime, and legacy state without stamps falls back to content hashes.
- **SQL lookup all rows**: the SQL path fetched a hard-coded 100,000 rows and silently
  truncated larger inputs; it now COPYs the join without a limit (2.3 GB drill: 19M/19M
  matched, 0 missing, 0.72 s, 244 MB RSS). Pipeline steps now record elapsed ms and
  `status --json` exposes hashes/timings.
- **`pipeline run`**: a CSV plan of steps reusing existing commands, with input/output
  chaining, `{out}` atomic temp+rename, stdout capture, `.pipeline.csv` input/args/output
  hashes, unchanged-step skipping and `--force`; failures write no output/state.
- **`convert --format`**: CSV/TSV/NDJSON/Parquet conversions via DuckDB in any
  direction; round-trip tests return byte-identical CSV, and xlsx is rejected while
  still disabled. 2.2 GB/55M rows: CSV→Parquet 5.36 s (224 MB, 581 MB RSS),
  Parquet→CSV 2.15 s (424 MB RSS), byte-identical result.
- **`analyze --quality`**: candidate keys, duplicate/empty key checks, mixed-type and
  numeric outlier findings (mean ± 3σ estimate), JSON (`quality.checks/findings`) or
  Markdown; `--group` adds group count/top share and dominance warnings.
  2.03 GB/56M rows: 18.71 s, 17 MB RSS.
- **`book --plan`**: a CSV manifest declares sheets, chained lookups and outputs;
  `book --plan book.csv --root .` materializes each output, reruns idempotently,
  and rejects unknown sheets/lookups before writing. No `Sheet!A1`.
  2.1 GB/55M rows + two chained lookups: 3.1 GB output in 46-48 s, 15 MB RSS,
  idempotent rerun byte-identical.
- **`edit compact`**: folds the current journal into a snapshot while keeping the
  original base as `.base` and the old journal as `.journal.history`, so undo can
  cross the boundary (it restores both, then undoes the tail) and redo still works.
  Crash markers repair an interrupted swap; a redo branch blocks compaction/undo.
  1M rows + 1M ops: view 1.13 s/export 0.91 s -> 0.02 s/0.02 s after a 1.04 s compact.
  Compaction holds the journal lock; crash hooks abort at four swap windows and the
  next command recovers with edits applied once; exports before/after are byte-equal.
  2.2 GB/55M rows: compact 1.77 s (8 MB RSS), boundary undo 0.03 s, deep jump rebuilds
  the snapshot index in 0.82 s then 0.01 s; the old .idx is archived with .base.
  10M-op history (227 MB): compact 8.98 s, then view/export 0.02/0.03 s; exports byte-equal.
  Real ENOSPC on a 300 MB volume leaves base/journal byte-identical and removes the temp;
  a waiting concurrent edit lands after the snapshot.
- **Byte predicates for parallel scans**: search and no-output filter now check
  records via `visit_fields` instead of allocating `String` fields; search also
  rejects a record whose raw bytes cannot contain the query before splitting it.
  11 GB/280M rows: search 17.09 s -> 2.62 s, filter 15.88 s -> 3.58 s, RSS 77 MB.
  31 GB/780M rows: index 11.11 s, search 7.38 s, filter 10.13 s, filter -o 7.81 s,
  RSS 81 MB, so the 30 s budget now covers at least 30 GB.
- **Parallel filter output**: UTF-8 files are split on line boundaries; workers match
  independently and write ordered part files that the main thread merges. 11 GB/280M
  rows: `filter -o` no-match 2.61 s, 56M matches with a 2.2 GB output 5.26 s (RSS
  17–28 MB), down from 14.9 s.
- **`networkdays(start, end)`**: counts Monday–Friday days with both ends included
  (no holiday list in the two-argument form); an end before the start is `#VALUE!`.
- **`weeknum(date)`**: the week number with Sunday as the first day (Excel WEEKNUM's
  default); Jan 1–4 2026 all read 1 and Jan 5 reads 2. Dates stay ISO text.
- **Text helpers**: `textjoin(sep, ignore_empty, ...)` joins values with a separator
  (and can drop empty ones), `char(n)` turns a code point into text, and `code(text)`
  returns the first character's code.
- **`iserror` / `isna` / `istext`**: `iserror(x)` is true for `#VALUE!`-style errors
  and for the text `#N/A` that ref/vlookup return on a miss; `isna(x)` is the narrow
  test that only accepts that `#N/A` spelling; `istext(x)` distinguishes a text
  value from a number. Excel's ISNA/ISERROR split is preserved, with the deliberate rule
  that a lookup miss counts as an error because it is spelled as text here.
- **`xlookup`**: `xlookup(sheet, key, value, fallback)` is ref/vlookup plus an
  if-not-found value; a miss returns the fallback expression (a literal or a column
  reference such as `default_name`) instead of the text `#N/A`, which `iferror`
  cannot catch.
- **`vlookup` alias fixed**: the evaluator treated `vlookup` like `ref`, but the
  reference loader only recognized `ref`, so a formula that used only `vlookup`
  never loaded its sheet. Both spellings now feed the loader.
- **Row-local multi-criteria aggregates**: `sumifs(A2:C2, D2:F2, ">0")` and the
  countifs/averageifs/minifs/maxifs forms pair same-row ranges position by position;
  widths must match, and a sum with no matching numeric cell is 0 while
  average/min/max are `#VALUE!`.
- **`sumproduct` over any number of ranges**: `sumproduct(B2:C2, D2:E2, F2:G2)`
  multiplies each row position across every range and adds the products; two ranges
  still behave as before, and different widths are `#VALUE!`.
 `sumproduct(B2:C2, D2:E2)` multiplies two same-row ranges
  element-wise and adds them (weighted totals); text cells count as 0 and a length
  mismatch is `#VALUE!`.
- **Wildcard criteria**: text conditions in `countif`/`sumif`/`averageif` (and the
  `*ifs` family) understand spreadsheet wildcards — `"*CDMX*"`, `"CD*"`, `"CDM?"` —
  case-insensitively, instead of treating `*` as a literal. Matching is an
  allocation-free glob, so a million-row condition does not compile a regex per row.
- **Date arithmetic**: `edate(date, months)` and `eomonth(date, months)` shift an
  ISO date (clamping a month-end to the shorter month), `weekday(date)` is Sunday=1 …
  Saturday=7, and `days(end, start)` is the signed day difference. They return ISO
  text, so one result can feed the next formula without a serial-number round trip.
- **`averageifs`**: `averageifs(amount, city, "CDMX", qty, "<5")` is the mean of the
  matching values; it reuses the set's sum and count, so no extra pass (no match is
  `#VALUE!`).
- **`minifs` / `maxifs`**: `minifs(amount, city, "CDMX", qty, "<5")` and `maxifs(...)`
  return the smallest/largest value among rows matching every criterion. The set keeps
  sum, count, min and max from the same pass; the `.agg` cache moved to version 6.
- **Same-row ranges**: `sum(B2:C2)`, `count(B2:C2)`, `average(B2:C2)` — and the conditional
  `countif(B2:C2, ">5")` / `sumif(B2:C2, ">5")` — read the cells between two columns of
  the current row. A range that needs another row still returns `#VALUE!` because a
  streaming pass cannot see the future; `B2` alone stays a single-cell reference. A
  criteria literal is compiled once at parse time, not once per row.
- **Atomic `transaction` writes**: `POST /edit?action=transaction&ops=set:1:city:CDMX;delete:2:1;insert:4:a,b`
  validates every entry against the sheet first, then appends them in one journal update.
  A rejected entry writes nothing; undo still steps one op at a time. Two rows of one
  request cannot land half-applied.
- **Resumable `/filter` and `/search` pages**: a whole-table scan that hits the 30 s
  budget no longer answers 504. The waves that finished are returned with a `next`
  checkpoint; passing it back as `cursor` continues where the scan stopped, and the row
  numbers still line up with `/rows`. UTF-8 parallel path only — GBK/UTF-16 and small
  files keep the single-shot scan, so they can still 504 on a truly huge file.
- **`scripts/drill.sh`**: one command that indexes, opens, jumps, filters, searches and
  (with `--writes`) edits, exports and verifies a real file, printing the markdown table
  the README quotes. The 11.2 GB row above was produced by it.

## [4.0.0] — 2026-11-20

### Changed

- **The project is now `csvx`.** One binary, one name: `csvx`. Everything that said
  `csv-splitter` says `csvx` — crate names (`csvx-core` / `csvx-cli`), the binary, release
  artifacts, the systemd unit (`deploy/csvx.service`), the demo directory (`csvx-demo/`),
  and every documented command. There is no compatibility alias: a rename that keeps two
  names never finishes.
- **Environment variables are `CSVX_*`**: `CSVX_STRICT_PARAMS`, `CSVX_QUERY_TIMEOUT_MS`,
  `CSVX_SQL_MEMORY_MB`, `CSVX_SQL_THREADS`, `CSVX_SQL_TEMP_DIR`. Scripts and tests moved
  with them; the strict-params guard the docs publish still holds under the new prefix.
- **README leads with the positioning**: *High-performance CSV editor in Rust*, a Rust badge,
  and a download table rebuilt around the new artifact names.

Major version because the breaking surface is the name itself: a shell history, cron job or
systemd unit that calls `csv-splitter` needs one edit. No behaviour changed.

## [3.62.0] — 2026-11-20

### Added

- **Sixteen more spreadsheet functions**, so a formula column is closer to what a spreadsheet
  user reaches for: `sign`, `power`, `exp`, `ln`, `log(x[,base])`, `roundup`, `rounddown`,
  `exact`, `substitute`, `find`, `search`, `proper`, `rept`, `value`, `concatenate`,
  `date(y,m,d)`. Directed rounding follows Excel (away from / towards zero for negatives),
  `find` is case-sensitive and `search` is not, both return 0 when absent, and `date` refuses
  an impossible date instead of printing one. A test walks each function and also checks that
  `date` composes with `year`/`month`/`day`.

- **`docs/BEYOND_EXCEL.md`**: the honest case for surpassing Excel — eight measured advantages
  with evidence (row limit, first screen, concurrent writers, machine-readable contract,
  journal auditing, read-time formulas, dual engines, verified split/merge), six named
  remaining gaps with acceptance criteria, and the non-goals. No claim without a number or a
  test behind it.

## [3.61.8] — 2026-11-20

### Tests / Documentation

- `set-many` edge cases are pinned instead of assumed: two entries naming the same cell apply
  in order (last wins), a value may contain colons, an empty value clears the cell, and a `;`
  inside a value becomes a malformed entry that refuses the whole batch before anything is
  written.

## [3.61.7] — 2026-11-20

### Added

- **`POST /edit?action=set-many&cells=row:col:value;…`** writes every cell in one journal
  update: a crash leaves either all of the cells or none, which per-cell requests cannot
  promise. Any invalid entry refuses the whole batch before writing, and undo still walks one
  cell at a time — the response carries `"undo":"one cell per undo"` so the boundary is stated
  rather than implied. Values may contain `:` but not `;`.

## [3.61.6] — 2026-11-20

### Added

- **`/capabilities` publishes `applyEdits`**, the list of routes that honour
  `apply-edits=1`, plus `stamp=journal-precondition` in `writeGuards`. A front-end that wants
  to show the edited table had to discover per route which ones support it; now it can read
  the list. A test asserts every published route is served by /spec.

## [3.61.5] — 2026-11-20

### Tests / Documentation

- Materialising endpoints snapshot the journal once, and a concurrency test now proves the
  consequence: thirty edits land while fifteen `/export?apply-edits=1` calls run, and every
  export is the whole table with a value that was actually written. An edit arriving after
  the snapshot is simply absent; a half-applied table is never produced. The API note states
  the boundary instead of leaving it to be discovered.

## [3.61.4] — 2026-11-20

### Tests

- The read side now has a concurrency test of its own: 40 edits land while 120 rounds of
  /rows and /journal are read, every response must parse, an edited answer must report
  `damaged:0`, and after the burst the last value and all 40 operations are visible. Three
  consecutive runs passed. The eight-writer test covered writes; this covers readers.

## [3.61.3] — 2026-11-20

### Tests

- Hostile cells (doubled quotes, backslash, embedded newline, single quote, `0x01`) are now
  checked through four more output paths: /export (CSV), /query, /groupby and /chart (JSON).
  All were already correct — sharing a helper with /rows is not the same as calling it, so each
  path gets its own assertion.

## [3.61.2] — 2026-11-20

### Fixed

- **`/rows?format=csv` was silently ignored on a sheet without edits.** The CSV branch lived
  only in the edited path, so a pristine file answered JSON despite the parameter — exactly the
  silent fallback the API promises not to do. Both paths now share one renderer, and a test
  pins the pristine CSV body plus quoting of quotes, backslashes and embedded newlines.

### Tests

- JSON escaping is now covered by real CSV content: a cell with a newline and another with a
  `0x01` byte must appear as `\n` and `\u0001`, because a raw newline inside a JSON string
  makes the response unparseable for every client.

## [3.61.1] — 2026-11-20

### Added

- **Per-route status distribution in /metrics.** The global `status` map answers "how many
  400s"; a route entry now also carries `status:{"200":n,"400":m}`, so a failing route is
  identifiable without guessing from the aggregate. Tested on a route that answered both.

## [3.61.0] — 2026-11-20

### Added

- **Per-route latency percentiles and error counts.** /metrics already had count/avg/max per
  route; an average hides the route that is fast until p95. Each route now carries `p50Ms`,
  `p95Ms` and `errors`, computed from the same fixed bucket layout as the global histogram,
  so the memory cost stays one small array per route. A test checks the fields appear.

## [3.60.7] — 2026-11-20

### Added

- **Every split archive now carries `_manifest.txt` (`chunks=N`, `rows=M`), and merge checks
  it.** Numbering proves the parts are contiguous; the manifest proves the rows came back.
  A chunk damaged in transit, or a part silently dropped, now fails with
  `the manifest says N row(s), the archive merged M` and the half output is deleted. Archives
  written before this change carry no manifest and still merge (the check is skipped).

## [3.60.6] — 2026-11-20

### Added / Fixed

- **`merge --chunks N` (CLI) and `chunks=` (/merge)** enforce the expected part count on top of
  the gap/repeat check: a re-split glued onto an older archive is a real way to get a longer
  archive, and the numbering alone cannot see it. A mismatch fails and deletes the half output.

- **Errors are no longer silent** for merge, sort and serve: all three returned an error without
  sending a progress event, so `exit 1` came with no message. A test now asserts that a failing
  sort names the missing column.

- **AppleDouble entries are skipped.** A macOS tar can carry `._t_0001.csv` resource-fork
  siblings; they look like chunks by name and made merge fail with a bogus "out of sequence"
  error. Tar, zip and directory inputs now ignore them.

## [3.60.5] — 2026-11-20

### Fixed

- **Merging a gap used to drop rows silently.** `merge` concatenated whatever chunks it found,
  in name order, so an archive missing `stem_0002` produced a file that looked complete and was
  not. The merger now tracks the expected chunk number and fails on a gap or a repeat, deleting
  the half-written output; a test covers both the refusal and the in-order success.

- The output file is excluded from the directory listing, so merging into a path inside the
  input directory no longer merges the output into itself (the new test caught that too).

## [3.60.4] — 2026-11-20

### Tests / Documentation

- /schema without  already applied edits (it shares the columns handler); with 
  it compares two files on disk. A test pins both halves: after a journal-only rename, /schema
  shows the new column while the diff against an unchanged file reports . The
  support matrix now distinguishes the two.

## [3.60.3] — 2026-11-20

### Tests / Documentation

- Validation's violation row numbers follow edits, and a test now pins it: after deleting the
  first row, the row that was 3 reports as 2 and the deleted row stops reporting. The API row
  says explicitly that `violations[].row` is the edited-sheet position, the same numbers
  `/rows` uses.

## [3.60.2] — 2026-11-20

### Measured / Documented

- Row-order evidence widened: at 200 000 and 2 000 000 rows, a head `LIMIT 5` and a tail
  `OFFSET` query both still return file order. **Today**, not a guarantee — which is exactly why
  `/filter` and `/search` row numbers still come from the native scan.

- Recorded the second blocker for SQL pagination: both endpoints report `scanned`, and a SQL
  path would either need a second `count(*)` scan or a nullable field. Written down so the next
  person does not rediscover it the hard way.

## [3.60.1] — 2026-11-20

### Added

- **`POST /copy` — the "save as" the front-end lacked.** `/export` runs a query; this is a
  whole-file duplicate, and with `apply-edits=1` the duplicate is the edited table instead of
  the base bytes. A test copies a sheet, edits it, copies again, and checks the second copy
  carries the edit while the first does not.

## [3.60.0] — 2026-11-20

### Documentation

- The apply-edits support matrix in one table: which endpoints apply edits by default (render
  and validation paths), which take `apply-edits=1` (analysis, export, sort, filter, search,
  lookup), and which deliberately do not (dedupe/split/partition/columns/concat/merge, whose
  semantics are copying or partitioning the source file). The rule in one line: endpoints that
  read a table and answer a reading apply edits; endpoints that emit a file copy do not.

## [3.59.9] — 2026-11-20

### Added

- **`POST /edit` accepts `stamp=` as a journal-wide precondition.** Cell-level `expect=` covers
  one cell; two edits based on one read had no way to notice the state moved. An edit carrying
  a stale stamp now answers 409 with the current stamp, and a successful edit returns the new
  stamp, so a client can retry safely. This narrows the "two edits are not a transaction"
  limitation without pretending to remove it.

## [3.59.8] — 2026-11-20

### Added

- **/sort accepts apply-edits=1**, so "edit a column, then export it sorted" works: the job
  materialises the edited sheet and sorts that, and the output is a flattened view of the edits
  rather than a copy of the base file. A test edits a value, sorts, and checks the output has
  the new value and not the old one.

- The aligned-concat engines now have an explicit parity test: with `by-name` both engines
  receive the same aligned inputs and their outputs are compared byte for byte.

## [3.59.7] — 2026-11-20

### Added

- **`concat --by-name` (CLI) and `byName=1` (/concat).** Files with the same columns in a
  different order used to fail on a header mismatch. The new flag aligns them by name first:
  only the order may differ, and a different column set is still an error (filling gaps with
  empties would invent data). Misordered inputs are rewritten to a temp file per file, so the
  fast byte-copy path still applies afterwards. Tests cover both engines' inputs: CLI checks
  the aligned bytes, HTTP checks the job's output, and both check that a different set fails.

## [3.59.6] — 2026-11-20

### Added

- **/filter and /search say which source they scanned**: `source` is `file` or `edited`. A caller
  that gets row numbers needs to know whether they index the base file or the edited sheet, and
  the flag alone made that implicit. Tests pin both values, and the documented-field guard
  covers the new key.

## [3.59.5] — 2026-11-20

### Added

- **/lookup accepts apply-edits=1.** It joins the main file against a reference; with edits the
  main side is materialised first, so a changed key joins against the reference instead of the
  original one. The test edits a key to ZZ9 and checks the copied region arrives.

## [3.59.4] — 2026-11-20

### Added

- **/analyze and /stats accept apply-edits=1**, so profiling follows a renamed or added column
  instead of describing the base file. A test renames amount to total: /analyze names the old
  column without the flag and only the new one with it, and /stats agrees.

## [3.59.3] — 2026-11-20

### Added

- **/filter and /search accept apply-edits=1.** Both scan the base file by design; with edits
  applied, a deleted row still matched and the returned row numbers disagreed with /rows after
  an insert or delete. With the flag they scan the edited sheet, so row numbers line up with the
  window a caller pages. Tests cover both directions: the base answer keeps the deleted row, the
  edited answer does not (filter checks the row numbers, search checks the match count).

## [3.59.2] — 2026-11-20

### Added

- **/export accepts apply-edits=1.** Exporting edited data was impossible: the endpoint turns
  the filter into SQL and DuckDB reads the base file, so a filtered export of an edited cell
  came back empty. It now materialises the edited sheet first when asked. A test exports
  where=city=ZZ9 with and without the flag: the edited value appears only with it.

## [3.59.1] — 2026-11-20

### Added

- **/chart and /pivot accept apply-edits too**, completing the trio: each materialises the
  edited sheet on a cache miss and analyses that. One test now covers groupby, chart and pivot:
  after an edit moves a row's city, all three answers omit the old city with the flag and keep
  it without.

## [3.59.0] — 2026-11-20

### Added

- **/groupby accepts the apply-edits parameter.** The CLI could analyse the edited sheet; the HTTP
  endpoint always read the base file. It now materialises the edited sheet once per cache miss
  and groups that, so "add a formula column, then aggregate by it" works over HTTP too. A test
  edits a value across groups and checks the grouped answer changes with the flag and does not
  without it. Capsule: pivot and chart still lack the HTTP flag.

## [3.58.9] — 2026-11-20

### Fixed

- **/analyze documented a parameter the handler never reads.** The row said distinct, but the
  handler reads distinctCap; under the default strict mode a client following the docs got 400.
  The row now names the real parameter and the per-column fields the answer carries, and
  /analyze joined the documented-field guard (eighteen routes).

## [3.58.8] — 2026-11-20

### Tests / Documentation

- The documented-field guard now covers seventeen routes, including /files, /book, /layout,
  /validate, /stats and /schema. Two mechanics were needed: a route may answer different
  shapes (schema with and without diff), so fields are asserted against the union of its
  answers; and every covered route must name at least one field, or the row would be silently
  checking nothing.

## [3.58.7] — 2026-11-20

### Fixed

- **Two more implemented-but-undeclared parameters**: chart desc and style start/count. Each is
  documented, each is read by its handler, and each answered 400 under the default strict mode.
  Both declared now, with a test that sends them and expects 200.

- The documented-field guard covers groupby, pivot and chart: columns/rows/engine,
  columns/rows/rowKeys, and type/rows/categories/series are asserted against live bodies.

## [3.58.6] — 2026-11-20

### Tests / Documentation

- The documented-field guard covers /filter and /search, and it now skips parameters the route
  declares — otherwise every row with a parameter column would fail. Both rows name their
  response fields (scanned, matched, truncated, columns, rows) instead of leaving them implicit.
  Mutation-checked by renaming truncated in /filter's body.

## [3.58.5] — 2026-11-20

### Tests / Documentation

- The documented-field guard covers /cell: row, column, value, key and total are asserted
  against the live body. The API table now names them; mutation-checked by renaming key, which
  the guard reports by name.

## [3.58.4] — 2026-11-20

### Tests / Documentation

- The documented-field guard covers /journal: operations, count, redo, redoCount, canUndo,
  canRedo and damaged are asserted against the live body. The API table had only said "operation
  stack plus redo stack plus damage count"; mutation-checked by renaming canUndo, which the
  guard reports by name.

## [3.58.3] — 2026-11-20

### Fixed

- **Dead declarations removed**: schema json, edit width/height and groupby apply-edits were
  declared parameters nothing read — a promise the server silently ignored. apply-edits is a
  CLI flag, json is meaningless on an endpoint that already answers JSON, and width/height were
  leftovers. The reverse contract test fails on any declared-but-unread name; mutation-checked
  by re-adding json, which it reports as ["json"].

## [3.58.2] — 2026-11-20

### Fixed

- **Three more parameters were implemented but undeclared**: /index interval, /partition
  maxPartitions and /split level. Each was documented and read by its handler, and each was
  answered 400 by the default strict mode. All are declared now.

- A contract test scans the served source for literal parameter reads and fails when one is
  absent from the spec table. Mutation-checked: removing interval from the table fails it, naming interval.

## [3.58.1] — 2026-11-20

### Fixed

- **/filter's limit was implemented but not declared**, and strict parameter checking is on by
  default, so the documented and working parameter answered 400 "unknown parameter limit".
  It is in the spec now, and a test pins the whole contract: matched counts every hit, limit
  caps rows, truncated says the list was cut.

## [3.58.0] — 2026-11-20

### Documentation

- **Windows is covered by CI, and the docs said it was untested.** The Windows check workflow
  builds on windows-latest for Rust changes, and the run for 6e910d5 succeeded. Limitation 6
  now states what the workflow does (and its path filter, so a docs commit does not pay ten
  minutes of DuckDB compilation), instead of claiming nobody has tested Windows.

## [3.57.9] — 2026-11-20

### Documentation

- The journal-visibility table gained a third category. /meta is neither edit-aware nor
  base-only: it reports the file's physical facts and an edited count, and points at /rows and
  /columns for the edited view. Written down so the 2-column/3-column contrast stops looking
  like a defect.

## [3.57.8] — 2026-11-20

### Added

- **/meta now has an edited count.** Its rows, bytes and headers are the base file's on
  purpose — physical facts — but staying silent about an existing journal made a 2-column /meta
  next to a 3-column /columns look like a bug. The field names the reason, the docs say where
  the edited view lives, and a test pins both halves: headers stay the file's, the counter tracks
  the journal, and the new column appears only in /columns.

## [3.57.7] — 2026-11-20

### Tests

- /columns is edit-aware and now says so: after renaming a column the list carries the new
  name and not the old one. A front-end drawing headers from a stale schema is the failure this
  guards against, and the journal-visibility table gained a row.

## [3.57.6] — 2026-11-19

### Tests

- The documented-field guard covers /sheet: path, indexed and indexBytes are asserted against
  the live body, the same way /health, /meta and /metrics already were.

## [3.57.5] — 2026-11-19

### Documentation

- Narrowed limitation 6 with evidence: the built-in viewer answers GET / with 200 and 13 KB of
  HTML whose title is correct, so "unverified" now means "no browser interaction was tested",
  not "nobody has ever seen it work". The probe is a two-line curl, reproducible offline.

## [3.57.4] — 2026-11-19

### Tests

- backend-smoke asserts the duckdb field on /health as well as on /capabilities: the probe a
  deployment points at should answer the build question too.

## [3.57.3] — 2026-11-19

### Documentation

- Removed the "80 rounds" counts from HANDOVER and the README. The number was written at a
  particular time, has no source anyone can check, and only drifts. The lesson tables say the
  same thing without a figure that will be wrong.

## [3.57.2] — 2026-11-19

### Tests

- smoke step zero checks that `--version` names whether DuckDB is compiled in. It is the first
  thing a user runs when an engine question comes up, and smoke is what a user would run.

## [3.57.1] — 2026-11-19

### Tests

- backend-smoke now asserts the two capability fields a client cannot work without:
  strictParams (true under the default) and the duckdb field. The unit tests covered the
  handlers; this is the check against a real `serve` process.

## [3.57.0] — 2026-11-19

### Added

-  reports strictParams. Strict mode changes how a request with extra parameters
  answers, and a client discovering the backend could not see which mode it was in. The test
  checks both states, default and CSVX_STRICT_PARAMS=0.

## [3.56.9] — 2026-11-19

### Documentation

- HANDOVER gained "change one thing, change this too": the guards added over the last rounds
  as a table of what fails when they are out of sync (journal lock, visibility matrix, engine
  parity, SPEC_PARAMS/writeOnlyParams, status codes, documented fields, changelog version).
  The verification row's counts were also stale and now match reality: 606 workspace tests,
  336 more under the sql feature, 49 smoke steps, 44 regress checks, 83 backend assertions.

## [3.56.8] — 2026-11-19

### Measured / Documented

- Re-measured concat's engine choice: 89 MB × 2 native 0.25–0.28 s, SQL 0.78–0.92 s. The
  "auto stays native" decision from earlier still holds at roughly 3×. Not measured at 2.5 GB:
  concatenating that fixture means writing 5 GB, and the gap is wide enough that a doubling of
  file size would not flip it.

## [3.56.7] — 2026-11-19

### Fixed

- **concat's header mismatch was 422 on the native engine and 400 on SQL.** The test that
  closed the last error-parity gap found it: native reported corrupt data (InvalidData) while
  SQL reported a bad request (InvalidInput) for the same condition. Native is InvalidInput now.
  With this, all five engine endpoints have both result and error parity tests.

## [3.56.6] — 2026-11-19

### Tests

- Error parity for lookup, the last two-file engine pair: a missing key on the main file and
  a missing key on the reference file both fail with the same error kind across engines. concat
  remains the only engine pair without an error-parity test, and it is now named as such.

## [3.56.5] — 2026-11-19

### Tests

- Error parity extended to groupby and pivot: a key column that does not exist fails with the
  same error kind on the native and SQL engines, for both commands. Both pass, so the maxGroups
  mismatch from earlier was isolated rather than systemic. lookup is the one remaining pair.

## [3.56.4] — 2026-11-19

### Tests

- **Error parity is now tested, starting with chart.** Result equality between the engines has
  tests; a request that must fail was not covered. Asking chart for a column that does not
  exist fails with the same error kind on both engines, and the SQL message names the column.
  The invariant table records that groupby and pivot still lack the same check.

## [3.56.3] — 2026-11-19

### Fixed

- **Going over the group cap is the same answer on both engines.** The native accumulator
  returned a plain io error (422) while the SQL path returned InvalidInput (400), so the same
  condition had two statuses. They now agree, and the 400 names maxGroups with a hint instead
  of the vague "request", because raising the cap is the actual fix.

## [3.56.2] — 2026-11-19

### Added

- **The spec publishes .** Two runtime guards (GET /edit, GET /columns) were
  refusing parameters that the spec declared for the whole route; a client generating calls
  from /spec had no way to know. The table now lives in one constant that both handlers and
  the spec use, so publishing it cannot drift from the behaviour.

## [3.56.1] — 2026-11-19

### Changed

- **GET /edit refuses action, row, col, value and expect.** Same root cause as GET /columns:
  one parameter list per route. A GET that carries write parameters and answers 200 reads as
  "the edit landed"; it now answers 400 and names POST /edit, with a test covering all five.

## [3.56.0] — 2026-11-19

### Changed

- **GET /columns refuses the four POST-only parameters.** The spec lists parameters per
  route, not per method, so keep/drop/rename/out are declared for /columns although only
  POST reads them; accepting and ignoring them made a write-shaped request look successful.
  They now answer 400 and name where they belong, and a test covers all four.

## [3.55.9] — 2026-11-19

### Documentation

- Contract audit recorded: every  parameter is really read ( and
   through , which a direct grep for the name misses), 
  carries a stable row key, and one structural known issue —  lists parameters per
  route rather than per method, so GET  accepts four POST-only parameters and ignores
  them.

## [3.55.8] — 2026-11-19

### Added

- **`/rows?format=csv` returns the window as CSV with edits applied.** Docs listed `format`
  while neither the handler nor the spec had it, so the documented parameter did nothing.
  It now exists and is declared, and it is the only way to get an *edited* window as text:
  `/export` reads the base file and a filtered export of an edited cell comes back empty.

## [3.55.7] — 2026-11-19

### Tests

- **Eight threads reading different windows at once.** They share the pager cache, so the test
  both proves each caller gets its own rows (the first cell must be that window's start id)
  and exercises the cache from several threads at the same time.

## [3.55.6] — 2026-11-19

### Fixed

- **HTTP edits now write under the journal lock.** The CLI used `update_locked`; the server
  loaded the journal, pushed, and saved, so two concurrent tabs could both load the same
  state and the later rename would drop the earlier edit. Set, undo and redo now go through
  `update_locked`, and a lock held too long answers 409 instead of a misleading 422.

  A test fires eight concurrent edits at one file and asserts all eight land in the journal
  and in the window.

## [3.55.5] — 2026-11-19

### Measured / Documented

- Measured the streaming download: server RSS 8.2 MB before, **10.2 MB peak** during a throttled
  89 MB transfer, and the received file matched byte for byte. The old whole-file read would
  have held the whole file, so "gigabyte exports do not blow up memory" is now a number rather
  than a hope.

## [3.55.4] — 2026-11-19

### Added

- **Single-range downloads (`Range: bytes=start-end`).** A multi-gigabyte export could not be
  resumed: the old handler sent the whole file or nothing. It now answers 206 with
  `Content-Range`, seeks to the offset and sends exactly the requested slice, so `curl -C -`
  works; an out-of-bounds range is a 416 that names the real length. Multi-range stays out —
  a resumable download needs one range, not a CDN.

## [3.55.3] — 2026-11-19

### Fixed

- **Download streams the file instead of reading it whole.** The comment on the handler said
  "stream a file under the root"; the code called `std::fs::read`, so a multi-gigabyte export
  became a multi-gigabyte allocation for one request. It now sends a fixed Content-Length and
  copies in 1 MB chunks, honouring HEAD. A test checks the body bytes and the length.

## [3.55.2] — 2026-11-19

### Measured / Documented

- Repeat-window latency on 2.5 GB: first request 21 ms, repeats 9.5 ms and 12 ms, and a
  different window 10 ms. The pager cache buys roughly half on the first hit; the sparse index
  does the real work. Recorded so nobody rewrites the read path to "warm the cache" — at
  ten-millisecond scale there is nothing there to win.

## [3.55.1] — 2026-11-19

### Tests / Documentation

- The documented-field guard now covers /health, /meta and /metrics. Two things came up:
  the table has two row shapes (`| `GET /route` |` and `| GET | `/route` |`), and most other
  endpoints use backticks for *query parameters*, so the check cannot be applied mechanically
  there. Both facts are recorded next to the test instead of being discovered again later.

## [3.55.0] — 2026-11-19

### Added

- **A test that documented response fields really exist.** It reads the API table, takes the
  backticked field names from a route's row, and asserts each appears in the live response.
  The first mutation test aimed at it renamed `bytes` in a *different* endpoint by accident
  and passed; renaming it in /meta failed the guard as intended, which is why the guard is
  worth keeping.

## [3.54.9] — 2026-11-19

### Fixed / Documentation

- The API table promised `/meta` returned a ragged flag and a column count; it returns neither.
  The row now names the five fields it really has, and points at `/validate` for row-width
  consistency. A test asserts those five keys and that no ragged field exists, so the promise
  and the answer cannot drift apart again.

## [3.54.8] — 2026-11-19

### Tests

- Validation is edit-aware and now says so: a rule that both rows break before an edit
  leaves exactly one violation after it. The visibility matrix listed validate as edit-aware,
  and this makes the claim testable instead of asserted.

## [3.54.7] — 2026-11-19

### Fixed / Documentation

- **The export endpoint was documented as seeing edits; it does not.** It turns the where
  clause into SQL and hands it to DuckDB, which reads the base file, so a filtered export of
  an edited cell returns an empty result. A visibility matrix test caught the wrong row.

- The matrix (the_journal_visibility_matrix_holds) now states both directions in one place:
  five endpoints that must show the edit, three that must keep scanning the base file.

## [3.54.6] — 2026-11-19

### Added

- **`/metrics` names the last damaged file.** A single global `journalDamaged` count says
  something is broken but not where; a per-file map would grow with every path a client
  touches. One bounded slot — `lastDamagedFile` — answers "which one, most recently" without
  letting the metric's cardinality run away.

## [3.54.5] — 2026-11-19

### Added

- ** sums .** A damaged journal is reported in the response that
  hit it, which helps the person looking at that window and nobody else. The metric accumulates
  the count across  and  loads so a dashboard can alert on "edits are being
  lost" instead of waiting for a user to mention it.

## [3.54.4] — 2026-11-19

### Tests / Documentation

- **`/style` now has a test that it judges the edited value.** It loads the journal, but a
  conditional-format endpoint reading the base file would paint cells by the pre-edit value
  while the sheet shows the new one. The test asserts an edit can make a cell start matching
  its rule, and the journal-visibility table gained a `/style` row.

## [3.54.3] — 2026-11-19

### Added

- ** reports ,  and which routes take an .** A client
  discovering the backend learned the limits but not that DuckDB might be absent or which
  endpoints could be asked for it. A test asserts the engine list and all five routes.

## [3.54.2] — 2026-11-19

### Added

- **`/health` reports `duckdb` alongside `ready` and `version`.** `--version` answers the
  build question on the command line; a probe or an operator in a browser had no equivalent.
  A test asserts the field is present, so a future build that drops the SQL engine cannot
  look identical to one that has it.

## [3.54.1] — 2026-11-19

### Added

- **`--version` says whether DuckDB is compiled in.** The most common first error after
  asking for `--engine sql` is "this build has no sql feature", and `--version` now answers
  that question before the error does. A test asserts the marker is present in either form.

## [3.54.0] — 2026-11-19

### Tests

- **The strict-parameter default and its escape hatch both have a test.** The earlier test
  enabled the check explicitly, which stopped proving anything once the default flipped: a
  test that sets the flag cannot tell whether the default works. It now asserts the default
  refuses an undeclared parameter and that `=0` lets it through.

## [3.53.9] — 2026-11-19

### Tests

- **Pivot and chart now have HTTP engine-parity tests**, the pair only lookup and concat had.
  Pivot compares the two bodies directly (order-stable); chart compares the point values with
  sorting, because its category order is allowed to differ by design.

## [3.53.8] — 2026-11-19

### Changed

- **`sort --engine` and `dedupe --engine` are refused with a reason**, instead of falling
  through to "unknown option" or, worse, being accepted and silently run natively. The
  message states the semantic reason (smart_cmp's numeric-only-when-both-parse rule for
  sort; first-occurrence needing a stable row order for dedupe), and both flags are listed in
  the command help as unsupported so `--help` tells the same story as the error.

## [3.53.7] — 2026-11-19

### Documentation

- **Collected the deliberate inconsistencies into one table.** `/filter` not seeing edits,
  `sort`/`dedupe` staying native, `concat --engine auto` never switching, chart ordering,
  the 8 MB threshold and strict-parameter mode each had their reason in a different section.
  Together, with the reason and the workaround for each, they stop reading as bugs.

## [3.53.6] — 2026-11-19

### Documented

- **Which endpoints see the journal, in one table.** `/rows`, `/cell`, `/journal`, `/sheet`,
  `/validate` and `/export` apply edits; `/filter` and `/search` do not — they are byte scans
  over the base file, and applying edits would mean materialising the edited sheet. The
  difference is deliberate and now pinned by a test: after an edit, `/filter` on the new value
  finds 0 while `/rows?where=` finds it.

## [3.53.5] — 2026-11-19

### Tests

- **The edit count agrees across the three endpoints that report it.** `/rows` carries
  `operations`, `/journal` carries `count`, `/sheet` carries `operations`; all three read the
  same journal, so a mismatch would mean one of them counts something else. A test makes two
  edits and asserts 2 from each, with a helper that fails loudly when the key is missing.

## [3.53.4] — 2026-11-19

### Tests

- **`/cell` is part of the render path and now has a test that it sees the journal.** The
  handler opens a `Sheet`, which loads the journal, but nothing asserted it: a window that
  showed edit A while a single-cell read showed the pre-edit value is the split-brain that
  makes a front-end look broken.

## [3.53.3] — 2026-11-19

### Changed

- **Strict parameter checking is now on by default.** An undeclared parameter answers 400 with
  `parameter`, `route` and a hint to `/spec`, instead of being silently ignored;
  `CSVX_STRICT_PARAMS=0` turns it off for clients that add their own extras.

  To get here the declaration table had to be completed. Turning the check on in the test suite
  flushed out a dozen parameters that are read through helpers by variable name — `count`,
  `from-row`, `to-row`, `cols`, `key`, `out`, `splits`, `ext`, `limit`, `root`, `desc`,
  `whereAny` — across `/edit`, `/export`, `/split`, `/files`, `/book`, `/groupby`, `/job`. All
  are declared now, and the full serve suite passes with the check enabled.

## [3.53.2] — 2026-11-19

### Added

- **Opt-in strict parameter checking (`CSVX_STRICT_PARAMS=1`).** With it on, a request
  parameter that `/spec` does not declare is refused with 400 naming it, instead of being
  silently ignored — which is how `has` and `sample` stayed invisible for so long.

  Why opt-in: turning it on by default flushed out a dozen legitimate parameters that the
  declaration table does not list because they are read through helpers (`count`, `from-row`,
  `out`, `splits`). Those are real capability gaps in `/spec`, and a default-on guard would
  reject working requests until the table is complete. The test covers both halves.

## [3.53.1] — 2026-11-19

### Fixed

- **`/stats` accepts `sample` and `/analyze` accepts `distinctCap`, and `/spec` now says so.**
  Both were read and ignored by the contract: a caller could not tell how much of the file was
  being sampled. `/stats`'s default is unchanged; the parameter is merely advertised.

  The source-scanning test from two rounds ago was removed after it failed to catch these two:
  it parsed the declaration table with a naive split and skipped routes it could not parse, so
  it was green while the gap existed. A guard that silently skips is worse than none.

## [3.53.0] — 2026-11-19

### Measured / Documented

- The doc claims `--buf-mb` is a total budget that does not multiply with chunk count; measured
  it. On 89 MB: `-n 16 --buf-mb 64 -t 4` peaks at **126 MB**, `-n 16 --buf-mb 8 -t 4` at
  **78 MB**, `-t 1` at 133 MB. If each chunk took its own 64 MB, 16 chunks would be ~1 GB, so
  the claim holds. CAPABILITIES also says what the claim does *not* mean: the process has its
  own ~70 MB baseline, so "budget" is the I/O buffer share, not total RSS.

## [3.52.9] — 2026-11-19

### Tests

- **split then merge reproduces the input byte for byte.** The merge module's own comment
  calls this "the cheapest way to prove a split is lossless", and nothing tested it end to
  end. The new test splits 1200 rows (quoted newlines every third row, non-ASCII names) with
  two threads and compares the merged file to the original byte for byte.

## [3.52.8] — 2026-11-19

### Measured / Documented

- `split -t` was documented as "try 4 threads on big files" with no number behind it.
  Measured on 89 MB with 4 chunks and default zstd: `-t 1` **447 ms**, `-t 4` **182 ms**
  (2.5x), and both runs produced **byte-identical** archives. CAPABILITIES now carries the
  numbers, the caveat about cold caches, and the note that small files gain nothing.

## [3.52.7] — 2026-11-19

### Tests

- **Closed the one unguarded claim from the audit:** parallel readers must not split a record
  that contains a quoted newline. The test writes 1200 rows (past the 1000-record checkpoint
  interval, so readers really start at different offsets), every third row carrying a quoted
  newline, and asserts four threads produce byte-identical parts to one thread.

## [3.52.6] — 2026-11-19

### Documentation

- Audited the comments that make absolute claims (`never`, `always`, `cannot`) and recorded
  which test guards each one. Six are covered; one is not: `split_parallel` says parallel
  readers never share a record, and only checkpoint behaviour is tested. The gap is written
  down with the exact experiment that would close it rather than left to be rediscovered.

## [3.52.5] — 2026-11-19

### Tests

- **Eight writers, one journal, every edit survives.** `update_locked` exists because two
  appending writers without a lock lose one edit silently (last rename wins), and that
  promise had no test. The new one runs eight threads through `update_locked` and asserts the
  file holds all eight operations, each with its own value.

## [3.52.4] — 2026-11-19

### Fixed

- **`/schema` no longer advertises `has`, which it never read.** A client sending `has=city`
  got 200 and no answer, because the parameter was declared and ignored. Existence checks
  live on `name=` (the `/columns` alias `/schema` shares), which is declared in its place.

  A source-scanning test for ghost parameters was attempted and removed: several keys are
  read through helpers that take the key as a variable (`resolve_out(params, "out")`,
  `number("count")`), so static scanning calls them ghosts and the test would have been a
  lie in the other direction.

## [3.52.3] — 2026-11-19

### Fixed

- **`/spec` was under-declaring parameters.** `/edit` advertised only `path` and `delim` while
  its handler reads fourteen keys; `/job` advertised neither `id`; `/warm` and `/export`
  omitted `sql`. A contract test now reads the dispatch table and each handler from the
  source and fails when a handler reads a key `/spec` does not list — proven by injecting a
  bogus read into a handler and watching it fail.

## [3.52.2] — 2026-11-19

### Fixed

- **Every 400 now names the parameter it is complaining about.** Nine did not: the request
  line, headers, an unknown compression format, an A1 range, and four engine "invalid input"
  arms that answered with a bare error string. The convention was documented and untested;
  a contract test now scans the source and fails when a 400 body has no `parameter`.

## [3.52.1] — 2026-11-19

### Changed

- **The gate names each stage and says which one failed.** Five commands used to run under
  one silent heading, so a red run left the reader grepping for the failure. Each stage now
  prints `== <name> ok ==` or `== stage <name> failed ==`, and `GATE_FAIL_TEST=1` injects a
  failing stage so `regress.sh` can assert both the exit status and the naming.

## [3.52.0] — 2026-11-19

### Added

- **`scripts/gate.sh` keeps the gate's exit status.** `bash scripts/dev.sh gate | tail`
  reports tail's status, so a failing clippy run reads as success to anything chained with
  `&&` — which is exactly how a broken commit got pushed one round ago. The wrapper captures
  the code, prints `== gate exit=N ==`, and exits with it; `regress.sh` asserts the wrapper
  still fails on demand, so it cannot rot into a yes-man.

## [3.51.9] — 2026-11-19

### Fixed

- **A crash between the journal and its redo sidecar can no longer double an undo.** Two
  files are not renamed atomically together, so the save order decides what a crash leaves:
  - redo empty: the redo file is removed *before* the journal is written, so a crash cannot
    leave a stale redo that the new edit was supposed to discard;
  - redo non-empty: redo is written first, so a crash leaves the undone operation in both
    files — and `load` reconciles that duplicate instead of offering it back twice.

  A test writes exactly that post-crash state (same op in both files) and asserts it loads as
  two operations with no redo, while a genuinely undone op still offers itself back.

## [3.51.8] — 2026-11-19

### Fixed

- **A torn journal record can no longer be applied as a short value.** Every operation is
  written with a trailing newline, but the reader accepted an unterminated final record if it
  happened to parse — `set,1,city,CDM` is a valid set-cell operation, so a crash inside the
  value would have written `CDM` where the edit said `CDMX`, silently. Reading now requires
  the terminator and counts the fragment as damaged. A test cuts the same record at every
  byte offset and asserts exactly one record is lost, at every cut.

## [3.51.7] — 2026-11-19

### Fixed

- **`concat --engine auto` no longer switches to DuckDB.** Concat is copy-dominated: native
  copies records and only parses headers, while DuckDB parses every column, writes the file
  and scans it again for the row count. Measured 18 MB x 2: 0.07 s native vs 0.20 s sql;
  90 MB x 2: 0.26 s vs 0.88 s. `resolve_for_copy` keeps `auto` on the native path, and an
  explicit `--engine sql` still works.

## [3.51.6] — 2026-11-19

### Measured / Documented

- Checked whether one threshold can serve every command: `chart` crosses over between 3.6 and
  9 MB (0.03 s native vs 0.04 s sql, then 0.14 s vs 0.07 s at 18 MB), and `pivot` between 3.6
  and 18 MB (0.05 vs 0.07, then 0.19 vs 0.11). Both land in the same 4–9 MB band as
  `groupby`, so the shared 8 MB threshold stands; pivot's two engines also produced
  byte-identical files at both sizes.

## [3.51.5] — 2026-11-19

### Changed

- **`--engine auto` now switches at 8 MB, down from 64 MB.** The old number had no
  measurement behind it; a bracket on `groupby` shows native and DuckDB cross over between
  roughly 4 and 9 MB (0.9 MB native 0.01 s vs sql 0.03 s; 18 MB native 0.16 s vs sql 0.07 s).
  Everything between 8 and 64 MB was quietly running the slower path.

## [3.51.4] — 2026-11-19

### Measured / Documented

- First screen and jumps were suspected of a full scan; measured on 2.5 GB / 684 917 rows /
  782 columns: `start=0` **0.014 s**, `start=50 000` **0.011 s**, `start=600 000` **0.016 s**,
  with a 5.4 KB sparse index. The read path needs no change, and CAPABILITIES now records the
  numbers plus the instruction to measure before rewriting it.

## [3.51.3] — 2026-11-19

### Added

- **`GET /search` stops at the same query budget.** That was the last synchronous scan with
  no brake: filter and validate already have one, and search reads the most rows per request
  because it compares every column. The 504 names `limit=` and `columns=` as the way forward.

## [3.51.2] — 2026-11-19

### Added

- **`GET /validate` honours the same time budget.** Omitting `limit` asks for the whole file,
  which on 2.5 GB is a tens-of-seconds scan nobody can cancel. It now stops at the budget and
  answers 504 with a hint to pass `limit=`; a small `limit` still completes (test covers both
  halves in one request pair).

## [3.51.1] — 2026-11-19

### Added

- **`GET /filter` honours a time budget.** A filtered scan is synchronous, so a client that
  gives up leaves the thread reading; a 2.5 GB scan is the difference between a page and a
  stall. `filter::scan_with_budget` stops with `TimedOut` and the endpoint answers 504 with
  the same message as `/query`: stopped, nothing running, narrow the where or page instead.
  A test uses a 1 ms budget on 200 000 rows to pin it.

## [3.51.0] — 2026-11-19

### Added

- **`repair <file>` fixes a damaged journal, on purpose and with a backup.** Reading already
  skips unreadable records, but a truncated tail kept reporting damage forever. `repair`
  copies the journal to `.journal.bak-<unix>` and rewrites only the readable records;
  `--dry-run` reports what would be dropped and writes nothing. A core test pins the three
  promises: the backup is byte-identical to the original, the repaired file loads with
  `skipped = 0`, and a dry run leaves everything untouched.

## [3.50.9] — 2026-11-19

### Tests / Documentation

- The damaged-journal behaviour now has a unit test: two complete records survive, the
  truncated one is counted, and the redo sidecar gets the same treatment. CAPABILITIES
  states the policy that goes with it — report, never silently repair, because rewriting the
  journal changes the user's edit history.

## [3.50.8] — 2026-11-19

### Added

- **`/metrics` reports `engines`.** Each analysis counts the engine that actually ran
  (groupby, chart, pivot, lookup, concat), so "is the SQL path being used" is measured
  instead of inferred. A test asks for native once and sql once and asserts both counters.

## [3.50.7] — 2026-11-19

### Documented

- Why `filter`/`search` keep their row numbers on the native path, with an experiment behind
  the claim: a 20 000-row file returns rows in file order from DuckDB today, both plain and
  filtered, but DuckDB promises no scan order across plans, versions or thread counts. The
  test (`a_duckdb_scan_returns_rows_in_file_order_here`) is a canary: if it ever fails, the
  native row numbers were justified, and if it keeps passing, they are still the only path
  with a guarantee.

## [3.50.6] — 2026-11-19

### Added

- **`POST /concat` takes `engine=auto|native|sql`**, the last endpoint that could not reach
  the engines its CLI counterpart could. Both engines produce the same file over HTTP in a
  test, and `engine=bogus` is a 400.

## [3.50.5] — 2026-11-19

### Added

- **`POST /lookup` takes `engine=auto|native|sql`.** The CLI could choose a lookup engine
  and the endpoint could not — same capability, two answers. A test submits both engines,
  waits for the jobs, and asserts the two output files are byte-identical; an unknown engine
  is refused with 400 rather than ignored.

## [3.50.4] — 2026-11-19

### Documentation

- `/filter` and `/rows` count differently on purpose — 1-based human row numbers vs a
  0-based window start — and the API table now says so. A test pins the link by filtering
  for the second row and asserting `start=1` returns exactly that row, so a later cleanup
  cannot drift them apart.

## [3.50.3] — 2026-11-19

### Fixed

- **Two 504 branches told the client different stories.** `/query` said "interrupted" and the
  filtered page said "exceeded its time limit", so a caller could not tell whether anything
  was still running. Both now share one message and one hint: the engine stopped, the
  connection closed, nothing keeps running, and retrying with a smaller scope is safe. A test
  covers the filtered-page branch, which had no timeout test before.

## [3.50.2] — 2026-11-19

### Fixed

- **`/spec` advertised six status codes while the server returns seventeen.** A client
  reading the contract could not know what 201, 202, 206, 304, 414, 416, 429, 500 or 503
  meant. The notes now live in one `STATUS_NOTES` table and `/spec` is built from it, and a
  contract test reads `reason_for` from the source and fails if any returned code has no
  note.

## [3.50.1] — 2026-11-19

### Documentation

- CAPABILITIES now records **two** reasons `sort` is not migrated, not one: the semantic one
  (`smart_cmp` compares numerically only when both sides parse, which no single SQL
  `ORDER BY` on a typed column reproduces, so mixed columns would sort differently) and the
  cost one (byte-level copy of non-key columns vs parsing 782 of them).

### Tests

- `/pivot` gets the same quoted-newline guard `/groupby` already had. The defect was
  identical in both, so the fix deserves the same coverage.

## [3.50.0] — 2026-11-19

### Fixed

- **`/groupby` split its own output by lines.** A grouped key containing a quoted newline
  became several rows, the same bug `/pivot` had. It now parses with the CSV reader.

### Added

- `/groupby` responses carry `"engine": "native" | "sql"` — which engine answered, rather
  than leaving the caller to infer it from timing.

## [3.49.9] — 2026-11-19

### Fixed

- **`/rows` reported nothing when the journal was damaged.** A journal cut off mid-write
  loses its last operation; the CLI already warned, and `/journal` already carried counts,
  but a window that silently showed the pre-edit value read as "my edit vanished". The
  response now carries `"damaged": N` (records that could not be read), and a test pins it
  by writing one good operation plus a truncated line.

## [3.49.8] — 2026-11-19

### Added

- **`concat --engine auto|native|sql`.** The SQL path builds one view over the file list and
  uses `COPY`; headers are still validated natively first, because DuckDB's `union_by_name`
  would happily put a column of amounts under a column of ids, which is exactly the
  corruption the native check exists to refuse. A test asserts identical output and an
  identical row count from both engines.
  - Cost paid for that count: the SQL path scans the inputs twice (COPY then `count(*)`),
    where the native path counts while copying. Recorded rather than hidden.

## [3.49.7] — 2026-11-19

### Added

- **`view --where … --order …` runs as one SQL query on large files.** The ordered read
  previously required no `--where` at all, so a filtered ordered page fell back to
  export + filter + external sort. Equality, inequality, contains, `in`, empty/non-empty and
  regex translate exactly (`COALESCE` keeps a blank field matching `empty`); the relational
  operators do not, because the native path compares numbers when both sides parse and
  strings otherwise, so those fall back with a printed note rather than a different answer.

## [3.49.6] — 2026-11-19

### Added

- **`lookup --engine auto|native|sql`, backed by a real `LEFT JOIN`.** The SQL module gained
  a two-file view (`t` + `ref`), so a lookup can be one join instead of a load-then-stream
  pass. Composite keys (`--key a,b`) work on both engines, an unmatched key still writes
  `#N/A`, and clashing output names keep the `_ref` suffix.
  - A test asserts byte-identical files from both engines, including the miss and the
    suffix, so "reuse the database" cannot quietly change a lookup's answer.

## [3.49.5] — 2026-11-19

### Added

- **`lookup --key a,b --from-key a,b` supports composite keys.** A single key on a table
  where `id` repeats silently attaches the first matching row: with `1,Puebla,SUR` and
  `1,CDMX,NORTH` in the reference, a main row `1,CDMX` came back as `SUR`. Parts are joined
  with the ASCII unit separator, so `a`+`bc` cannot collide with `ab`+`c`.

## [3.49.4] — 2026-11-19

### Added

- **`pivot --engine auto|native|sql`** for the file output path (the terminal matrix stays
  native, it is a three-column rendering rather than something to stream). A test asserts
  byte-identical files from both engines, including the `""` bucket an empty key produces
  and the "sum prints 0 where mean prints nothing" rule.

  Three things the database needed taught to match the native output: an empty CSV field is
  NULL to DuckDB but a real bucket to the native path, SQL rejects a zero-length alias while
  a CSV header accepts an empty name, and a cell with no contributing row is 0 for sum/count
  rather than NULL.

## [3.49.3] — 2026-11-19

### Fixed

- **DuckDB ran with its defaults: up to 80% of RAM and every core.** Every query now opens
  through one `open_limited()` that pins `memory_limit` (default 512 MB), `threads`
  (default: available parallelism, capped at 8) and `temp_directory`, so a large sort or
  hash spills to disk instead of taking the machine.
  - Overridable when the machine can afford it: `CSVX_SQL_MEMORY_MB`,
    `CSVX_SQL_THREADS`, `CSVX_SQL_TEMP_DIR`.
  - A test reads the settings back from the connection, so the limits are proved rather
    than documented.

## [3.49.2] — 2026-11-19

### Added

- **`chart --engine auto|native|sql`.** Below 64 MB the native streaming accumulator runs;
  above it the query goes to DuckDB through the same declared-schema path as `groupby`
  (2.2 s on the 2.5 GB / 782-column fixture).

  The two engines agree on every category's number, which a test asserts. They do **not**
  agree on category order: the native path keeps the order categories first appear in, so a
  date axis stays chronological, while `GROUP BY` has no such order and returns them sorted
  by label. That difference is deliberate and written down rather than papered over.

## [3.49.1] — 2026-11-19

### Fixed

- **`view --order` no longer sorts the whole file to show five rows.** It exported, filtered
  and external-merged 2.5 GB before handing back one page. With no pending edits and no
  `--where` there is nothing DuckDB cannot see, so the ordered read is `ORDER BY … LIMIT`
  through the same declared-schema path groupby uses.

  | 输入 | 之前 | 现在 |
  |---|---|---|
  | 2.5 GB / 782 列 / `--order … --count 5` | 29.8 s | **2.6 s** |
  | demo 20 120 行（低于 64 MB，仍走 native）| 0.05 s | 0.049 s |

  Same top-5 values as the native path (0.88225, 0.878669, 0.870121, …). The moment a
  journal edit exists the native path is used, because the database would read the base
  file without it.

## [3.49.0] — 2026-11-19

### Added

- **`--engine auto|native|sql` for `groupby`**, and the core `engine` module that will carry
  the rest of the analysis commands. `auto` picks DuckDB above 64 MB and the native reader
  below it, because the measurements point opposite ways at two sizes:

  | 输入 | native | DuckDB |
  |---|---|---|
  | demo 20 120 行 / 6 列 | **0.026 s** | 0.207 s |
  | 2.5 GB / 684 917 行 / 782 列 | 13.1 s | **2.3 s** |

  The large-file number goes through `sql::query_with` deliberately: that path declares
  every column from a 64-row sample, while `read_csv_auto` spends the time inferring 782
  types and lands at 10 s.
- A test asserts the two engines write **byte-identical** output on data both understand,
  so "reuse the database" cannot silently change a number.

### Notes

- Numeric aggregates over a column that mixes text keep SQL arithmetic on the `sql` engine
  (the text is ignored) where native reports `<mixed>`. Documented rather than hidden.

## [3.48.3] — 2026-11-19

### Fixed

- **An empty cell was coerced to the string "0" before any text function saw it.**
  `len(note)` returned **1** for an empty note, `upper(note)` returned `"0"`, and
  `isblank(note)` was false. The cause was a single rule: empty cells became
  `Value::Number(0.0)` so arithmetic would treat them as zero. A new `Value::Empty`
  keeps that arithmetic behaviour (`Empty` still numbers as 0) while rendering as `""`,
  so text functions see nothing: `len("") → 0`, `isblank("") → TRUE`.
- **In-place recompute was refused as a circular reference.** `edit set-formula --col
  amount --value "amount*1.1"` failed with "the formula reads its own column", which
  blocked the main reason the feature exists. The row pipeline builds every cell from the
  base row before overwriting the computed column, so reading your own column sees the old
  value and cannot recurse. It is allowed now; a **new** column that reads itself is still
  refused, because there would be nothing to read.

## [3.48.2] — 2026-11-19

### Fixed

- **External sort sized its chunks by row count, so wide files were fully resident.**
  The chunk was capped at 500 000 records: ~20 MB for a six-column file, but **~1.9 GB
  for the 782-column fixture**. Measured peak RSS on the 2.5 GB file: **2.2 GB** — the
  whole file in memory, which is what external sorting exists to avoid.
  - Chunks are now bounded by a byte budget (`--buf-mb`, default 64 MB), with a fan-in
    cap of 128 chunks so a small budget cannot create thousands of temp files.
  - Same file, `--buf-mb 32`: peak RSS **2.2 GB → 125 MB**, runtime 29.9 s → 33.8 s,
    output **byte-identical** (sha256 compared with the pre-fix run).
  - `view --order` peak RSS on the same file: 2.28 GB → 230 MB.

## [3.48.1] — 2026-11-18

### Fixed

- **`/lookup`'s reference path escaped the root.** It checked `exists` instead of
  canonicalising and comparing against the root, so `from=../../etc/passwd` was accepted
  on a Linux box — the macOS temp-directory layout pointed that path at nothing, so the
  test passed locally and CI caught it. It now goes through the same guard as a read, and
  the 404 says why: "not a file inside the served directory".
  - The test was also too lenient (`404 || 400`); it now asserts 404 and the message.

## [3.48.0] — 2026-11-17

### Fixed

- **`/spec`'s parameter table now covers every endpoint.** It listed 16 routes and omitted
  the rest (`/search`, `/filter`, `/pivot`, `/chart`, `/query`, `/sort`, `/dedupe`,
  `/partition`, `/split`, `/merge`, `/files`, `/book`, `/style`, `/layout`, …), and the
  `/edit` list was missing ten of the keys its handler reads.
  - A new test asserts both directions against the source: `/spec` may not describe an
    endpoint the server does not serve (a guessed `/eval` did, until this existed), and no
    served endpoint may be absent from the table.

## [3.47.0] — 2026-11-17

### Added

- **`validate --limit N`** on the CLI, matching the endpoint that gained it last round:
  the summary line and the JSON both say what was skipped
  (`2 rows checked of 4 (--limit), 1 violation(s)`).

### Known gap, recorded

- `/spec`'s `params` table is **incomplete**: handlers read more than twenty keys it never
  lists (`cols`, `rename`, `paths`, `q`, `sql`, `rules`, …). I tried to enforce agreement
  with a test and it produced too many false alarms — parameters are often read through
  helpers (`resolve_out(root, params, "out")`), so matching only `params.get(...)` reports
  reads as missing. Nearly all of those keys are documented in API.md; `/spec` lags behind
  it. That is written down in API.md as debt to pay per endpoint, not papered over with an
  allowlist.

## [3.46.0] — 2026-11-17

### Added

- **`GET /validate?limit=`** bounds a validation scan. Measuring the endpoint on the real
  2.5 GB file showed a full check costs **34.9 s** — acceptable for a batch job, wrong for
  a browser previewing a window. `limit=200` answers in **0.11 s**, and the report is
  explicit about what it did: `rows` is the sheet's size, `checked` how much was examined,
  `partial:true` when the two differ. `limit=0` is a 400 rather than a scan that checks
  nothing.

## [3.45.0] — 2026-11-17

### Fixed

- **`/capabilities` was missing `delete-where`** — the action was documented in API.md and
  handled by the dispatcher, but the machine-readable list a client reads did not mention
  it. Found by a new test that compares `EDIT_ACTIONS` against the actual match arms in
  `serve.rs` in both directions.

### Tests

- `crates/cli/tests/contract.rs`: the advertised action list and the dispatch must agree.
  It models the two things that make this non-trivial and documents them: three actions
  (`export`, `undo`, `redo`) are handled by early branches before the `match`, and four
  squashed spellings (`deletewhere`, `insertcolumn`, `splitcolumn`, `setformula`) are
  aliases that a client should never be told to use.

## [3.44.0] — 2026-11-17

### Added

- **`POST /convert`** — rewrite a file in another encoding, as a job: `from` defaults to
  auto-detection, `to` is `utf-8` or `utf-8-bom`. Same core `encoding::transcode` as
  `csvx convert`, so a GBK export in the served directory can become UTF-8 text a
  browser can parse without leaving the API. An unknown encoding is a 400 naming `from`.

### Tooling

- `scripts/check_match_arms.py` fails when a match arm label appears twice in `serve.rs`,
  and `regress.sh` runs it. The compiler rejects unreachable arms too — but only after the
  compile the author was trying to avoid, and only when the shapes are visible to it. Three
  replacement edits in a row left duplicate `fill` / `insert-column` / `delete-where` arms
  in this round's parent; this catches the class in about a second, from the sweep.

## [3.43.0] — 2026-11-17

### Added

- **Multi-column range fill**: `edit fill --cols a,b --from-row 1 --to-row 2 --value 0`
  and `POST /edit?action=fill&cols=a,b`. Editing a block of a spreadsheet is one gesture,
  so it is **one operation** — one undo covers every column, no matter how many the user
  selected. `Op::FillColumns` stores the columns once and expands into per-column spans at
  compile time, so the journal cost stays one line.
  - The fill still resolves to row identities, so deleting an earlier row moves it up with
    the rows it landed on rather than sliding onto a neighbour (asserted by test).

## [3.42.0] — 2026-11-17

### Added

- **Insert by row key**: `edit insert --after-key b:12` / `--before-key v:3` and
  `POST /edit?action=insert-after-key|insert-before-key`. A client holds identities from a
  previous `/rows` (`b:12`, `v:3`), not positions — positions move as rows are inserted.
  The server resolves the key against the sheet as it stands at write time
  (`workbook::position_of_key`, built on a new `RowMap::position_of`), so "insert after
  this row" means that row even if something was inserted above it meanwhile.
  - A key that no longer exists is 404 naming `key`; a malformed key is 400 with the shape
    it should have had. Silently appending to the end would be the worst behaviour, since
    it looks like it worked.

## [3.41.0] — 2026-11-17

### Added

- **Column recompute**: `edit set-formula --col C --value EXPR` and
  `POST /edit?action=set-formula&col=&value=` make an **existing** column computed, in
  place, as one journal operation — so one undo restores the original values. `Op::
  SetColumnFormula` is a new journal op, and it works on every read path at once because
  it joins the same formula compilation the appended columns use.
  - The formula wins over the base value *and over any cell edit in that column*. A
    half-computed column would be harder to explain than a fully computed one.
  - **Circular references are refused at write time**, by both entry points: a formula
    that reads the column it computes, or an unknown column name, is a 400 (and CLI exit
    2) before anything enters the journal — storing it would accept the request and
    break the next read. `core::workbook::check_formula` is the one implementation.
  - Verified on all three paths the engine reads through: export, window reads and the
    journal round trip (`setformula,col,expr`).

## [3.40.0] — 2026-11-17

### Added

- **`POST /columns`** — the write twin of `GET /columns`: `keep`, `drop`, `rename` produce
  a new file with a different column set, as a job. The source file is never touched, and
  the result is a separate file the caller names.
- **`action=formula`** on `POST /edit` — appends a computed column, matching
  `edit formula --name N --value EXPR`. It is one journal operation, so one undo removes
  the whole column rather than a row of it.
  - `action=split-column` was already reachable over HTTP; a test now locks that in, since
    the capability table had claimed it was CLI-only.

## [3.39.0] — 2026-11-17

### Added

- **`GET /schema?diff=<file>`** — the column-set comparison the CLI prints, over HTTP, with
  the **same JSON document**. The comparison moved into `core::schema::diff` and both
  entry points call it, so "which column moved" cannot mean two different things. Column
  lists come from the sampling path, so comparing two schemas never builds an index.
  - `same:true` with a non-empty `moved` means "same columns, different order": a routine
    reorder is not a schema change, and the endpoint says both facts.
- **`POST /concat`** — stack files that share a header, as a job. Every input and the
  output go through the path guard, so this endpoint cannot be used to concatenate
  `/etc/passwd` into something downloadable. A header mismatch fails the job instead of
  writing a ragged file.

### Fixed

- **`202 Internal Server Error`**, printed by every `curl -i` and proxy log next to a
  successful job submission. `reason_for` now knows 201/202/206/304/416/429/500/503 —
  the same class of bug as the old `414 Internal Server Error`.

## [3.38.0] — 2026-11-17

### Added

- **`GET /validate`** — the CLI's `validate`, on HTTP, returning the core `Report`'s own
  JSON so the two entry points cannot drift: `rows`, `violations` (row/column/rule/value),
  per-rule `counts`, `truncated`, `uniqueFull`, `ok`, and it applies the journal like every
  other read path — a cell you fixed in the grid is validated at its new value.
  - Bad data is **200 with `"ok":false`**, not a 4xx: the request was fine, the data is not.
  - An unparseable check is 400 naming `checks`, because guessing what the user meant is
    worse than saying which parameter to fix.
- **`POST /lookup`** — cross-file lookup as a job: `key` in this file, `from` reference,
  `get` columns to copy, `out` result. Unmatched keys are written `#N/A`, the same marker
  the CLI and the engine use everywhere else; the reference path goes through the same
  root guard as every other request.

## [3.37.0] — 2026-11-17

### Fixed

- **The ubuntu/mingw Windows cross-check could not work, and now it says so.** Its first
  run failed inside `libduckdb-sys` with `'mutex' in namespace 'std' does not name a type`:
  Ubuntu 22.04 ships mingw-w64 **GCC 10** with the *win32* thread model, whose libstdc++ has
  no `std::mutex`/`std::shared_mutex`, and DuckDB's headers use both. A cross-check that
  cannot compile the engine is not a cross-check.
  - `.github/workflows/windows.yml` now runs `cargo check -p csvx-cli` on
    **windows-latest with MSVC** — the target the release actually ships — still filtered to
    `crates/**` and the dependency manifests, and still annotating its own failures.
  - The local mingw build that succeeded used **GCC 14** (Homebrew), which has the posix
    thread model; that is why it looked fine locally and died on the runner. Both facts are
    now in DEPLOY.md and ACCEPTANCE.md rather than in this conversation only.

### Verified

- `Windows check` run 3 (`1095f33`) **passed** on windows-latest with MSVC, compiling the
  CLI and its whole dependency stack (DuckDB included) for Windows. That closes the last
  platform without a green CI signal; every push that touches `crates/**` gets it.

## [3.36.0] — 2026-11-17

### CI

- **A push-time Windows check that only runs when it can matter.** `.github/workflows/windows.yml`
  cross-checks `cargo check -p csvx-cli --target x86_64-pc-windows-gnu` on
  ubuntu with mingw-w64, filtered by `paths: crates/**, Cargo.toml, Cargo.lock` — because
  that costs about five minutes of DuckDB C++ compilation for the Windows target, and a
  documentation commit should not pay it. The MSVC check on windows-latest stays for pull
  requests and manual dispatch, where the real release target is the one that matters.
  - The plan in the objective was `cargo check --target x86_64-pc-windows-msvc` on ubuntu;
    that cannot work (the `-sys` build scripts need a platform toolchain), so mingw is the
    honest substitute, documented as such.

## [3.35.0] — 2026-11-17

### Verified

- **Windows compiles.** `brew install mingw-w64`, `rustup target add
  x86_64-pc-windows-gnu`, and a cross build with the mingw linker finished in 4m17s:
  `PE32+ executable (console) x86-64`, 91.7 MB, packaged as a 25.0 MB zip with a checksum.
  That exercises the `cfg(unix)` gating for signals and symlinks across the whole
  dependency stack, DuckDB included — the gap that could previously only be discovered at
  tag time.
  - What this does **not** prove: that the `.exe` runs. There is no Wine here, so it is
    "compiles", not "works". The run evidence still comes from `windows-check` or the
    release matrix on a real Windows runner.
  - The release matrix uses MSVC, not GNU; a successful mingw build is strong evidence
    that our own code is platform-clean, not a substitute for the MSVC build.

## [3.34.0] — 2026-11-12

### Docs

- **DEPLOY.md gained a "when it will not start" table**: seven symptoms with the cause and
  the fix — a unit that dies because `ExecStartPre` failed, a taken port, `ready:false`,
  a path the server refuses, a 1.4 s first page that means "no columnar cache", a 504 that
  means the query budget. Deployment pages usually describe the happy path and leave the
  operator to grep logs.
- The `serve --check` examples now include a **failing** run beside the passing one, and a
  test (`serve_check_output_matches_deploy_doc`) pins the shape of both against the real
  binary's output. It deliberately ignores the version number, which changes every
  release, and pins everything else: a doc that quotes output is a doc that has to keep
  quoting it correctly.

## [3.33.0] — 2026-11-12

### Added

- **`scripts/release-decision.sh`**: the release workflow's three-way decision (tag push,
  dispatch with a tag, dispatch without one) moved out of inline shell and given a
  `--self-test` with seven cases, including the two refusals a workflow should have:
  a non-tag ref, and a tag that disagrees with `Cargo.toml` — the second would publish a
  binary whose `--version` lies.
  - The workflow now calls it, and CI runs the self-test on every push, so the branch
    logic has executed before the first tag rather than during it.
  - `regress.sh` runs it too; a sweep that skips the release path is a sweep of
    everything except the thing that ships.

## [3.32.0] — 2026-11-12

### Verified

- **Intel macOS builds and runs from this arm64 machine.** `cargo build --release
  --target x86_64-apple-darwin` finished in 2m37s (65.5 MB binary), and
  `release-check.sh --target x86_64-apple-darwin` passed end to end: unpack, checksum,
  `--version`, `demo`, `serve --check`, `/health`, `/spec`, `/rows`, SIGTERM. So two of
  the four release-matrix entries now have local evidence; Linux is covered by every CI
  push, Windows is waiting for `windows-check`.
- `scripts/release-check.sh --target <triple>` packages and probes a chosen build target
  and names the artifact the way the release matrix does, so a matrix entry can be
  exercised before a tag exists.

### Docs

- DEPLOY.md explains the three ways the release workflow can be triggered and the order
  to use them: a `workflow_dispatch` run with an empty tag builds all four platforms
  without publishing, then a real tag publishes.

## [3.31.0] — 2026-11-12

### Added

- **`scripts/install-check.sh`**: the three documented install paths now each end with
  evidence. Source build, `cargo install --path crates/cli`, and the release artifact are
  each probed with `--version`, `demo`, and `serve --check` — a table of `ok` lines or a
  non-zero exit.
  - `cargo install` is attempted `--offline` first (the dependency cache is warm from the
    build) and falls back to a networked install exactly once on a clean machine, saying
    so. The first attempt failed here with "transfer too slow", which is what a clean
    cache looks like on a bad connection.
- `backend-smoke.sh` runs `serve --check` against the served directory **before** starting
  the server — the same command a systemd `ExecStartPre` uses — so the deployment
  self-check is covered by the HTTP regression too (79 checks).

## [3.30.0] — 2026-11-12

### Added

- **`serve --check`**: verifies a deployment without starting the service — version and
  OS, root exists and is readable, the address can be bound. It exits non-zero and names
  the failing check, so a systemd `ExecStartPre` or a deploy script can gate on it.
  A non-loopback address prints a warning rather than failing, because that is a
  deliberate choice this build has no authentication for.
- `deploy/csvx.service` uses it as `ExecStartPre`, so a bad root or a taken port
  fails the unit before it ever starts serving.
- `scripts/release-check.sh` now **verifies** the checksum instead of checking that a
  `.sha256` file exists, and runs `serve --check` from inside the unpacked artifact —
  the config check is exercised on the thing users download, not only on the source tree.

### Fixed

- The same script created its demo directory *after* checking it, so the artifact's own
  `--check` failed on a directory that did not exist yet. Order corrected.

## [3.29.0] — 2026-11-12

### Added

- **`scripts/package.sh`** produces the artifact a user downloads — `tar.gz` on Unix,
  `zip` on Windows, each with a `.sha256` — and **`scripts/release-check.sh`** then
  unpacks it in a clean directory and runs the three things the documentation promises:
  `--version`, `demo`, `serve` with `/health`, `/spec`, `/rows`, and a SIGTERM shutdown.
  - The release workflow now calls `scripts/package.sh` instead of packaging inline, so
    the locally verified packaging is the same code that runs at tag time.
  - The Windows steps write into `dist/` like the Unix ones; they had been writing to the
    repository root while the upload step expected `dist/`.
  - Measured on 3.28.0 / macOS arm64: 64.1 MB binary (DuckDB statically linked), 19.5 MB
    `tar.gz`, checksum alongside.

## [3.28.0] — 2026-11-12

### Build

- **The release workflow no longer installs GUI packages.** Both workflows still ran
  `apt-get install libgtk-3-dev libxdo-dev fonts-noto-cjk` — a minute per run for a GTK
  dependency tree the CLI never needed, left over from the removed GUI crate. Gone from
  both; the release cache key is rolling now, like CI's, instead of per-lockfile.
- **Windows is a plausible build again.** Signal handling is `cfg(unix)` with a no-op
  elsewhere (Windows has no SIGTERM to catch; Ctrl+C is what a console user expects), and
  the two unix-only tests (symlink escape, SIGTERM) are `cfg(unix)`. A `windows-check` job
  runs `cargo check -p csvx-cli` on windows-latest for pull requests and manual
  dispatch — not on every push, because MSVC plus bundled DuckDB costs about ten minutes.
  - An ubuntu cross-check with `--target x86_64-pc-windows-msvc` was the first idea and
    does not work: bzip2/zstd/lzma/duckdb have native build scripts that need a platform
    toolchain, so the check dies inside the `-sys` crates, before any of our code.

### Added

- **`docs/DEPLOY.md` and `deploy/csvx.service`**: installation by release
  artifact, `cargo install`, or source; the `serve` knobs and the one environment
  variable; a hardened systemd unit with `KillSignal=SIGTERM` and `TimeoutStopSec=15`;
  which files are data (`.journal`) and which are rebuildable (`.idx`, `.parquet`);
  upgrade and rollback as "swap the binary"; and an explicit security table whose last two
  rows say "no authentication" and "no multi-user isolation".

## [3.27.0] — 2026-11-11

### Added

- **gzip responses.** A 782-column window is 3.6 KB per row, so the audience for this
  backend (a browser) asks for compression and previously got none. Bodies over 512 bytes
  are gzipped when `Accept-Encoding` asks: a 500-row, 4-column window went from 22290 to
  5714 bytes (3.9×).
  - A compressed body is a different representation, so its validator carries a `-gz`
    suffix and `Vary: Accept-Encoding` is always sent; `If-None-Match` still revalidates to
    304 for the representation the client holds.
  - HEAD returns the gzip headers with no body.
  - The test reads the response as bytes (a gzip body is not UTF-8) and decompresses it to
    check it is the same JSON, not merely smaller.

## [3.26.0] — 2026-11-11

### Verified

- Full acceptance run on a fresh copy of the real 2.5 GB / 782-column file, with the
  numbers in `docs/ACCEPTANCE.md`: preflight 204, HEAD without a body, ETag → 304 in
  2 ms, `/spec` 13.6 ms, `/columns` 10 ms, `/meta` 374 ms, cold filtered first page
  302 ms, warm filtered first page 0.23–0.26 s, warm ordered page 0.30–0.37 s.
  - One row is marked ⚠ rather than ✓: the warm ordered page occasionally lands at
    0.37 s against a ≈0.35 s expectation. Reported, not rounded away.
  - The original file's sha256 is unchanged after the whole run; only `.idx` and
    `.parquet` sidecars appeared next to it.

## [3.25.0] — 2026-11-11

### Fixed

- **`504` was leaking the engine's own text.** A timeout answered
  `"detail":"INTERRUPT Error: Interrupted!"` — the last place where DuckDB's wording
  reached a user. Timeouts now carry `error` + `parameter` + `hint` only; the engine's
  message stays in the server's own error path (400 for a bad query still explains
  itself, because that message is about the caller's SQL).

### Added

- **`CSVX_QUERY_TIMEOUT_MS`** tunes the query budget (default 30 s), and
  `/capabilities` and `/spec` publish the effective `querySeconds` instead of a
  hardcoded 30. It is also how the timeout path is tested without waiting half a minute.

### Verified

- Robustness is asserted, not described: a client that hangs up mid-response (five times
  in a row) leaves the server healthy; 20 concurrent requests all answer correctly and
  `/metrics` counts them; a one-millisecond budget produces `504` with no `INTERRUPT`,
  `Binder Error` or `detail` in the body.

## [3.24.0] — 2026-11-11

### Performance

- **`/columns` went from 0.65 s to 8 ms** on the 2.5 GB / 782-column file. It was
  sampling through `Sheet::schema`, which needs the sparse index — so "list the columns"
  built an index over the whole file. Pristine sheets now sample directly
  (`schema_sample`), and the row count is `null` when no index exists: honest, rather
  than a full scan in disguise.
- `/meta` (which does need the count) is the one request that pays for the index: 0.24 s,
  inside the 0.5 s budget.

### Added

- **`/sheet` publishes the cost tier** before a client asks for a page:
  `"cost":{"tier":"cold","filterFirstPageMs":1000,"orderFirstPageMs":1400,"hint":"POST
  /parquet … moves this sheet to the fast tier"}`. The numbers are the contract's
  numbers, not a measurement of the moment; the tier flips the moment the cache exists.
- `docs/ACCEPTANCE.md` now carries the two-tier table (cold vs warm) with the measured
  numbers for every call it promises.

## [3.23.0] — 2026-11-11

### Added

- **Job cancellation that actually stops work, and says so when it cannot.**
  `POST /job?action=cancel&id=…` answers 202 (accepted, poll until `cancelled`), 200
  (already finished — nothing was stopped), 409 (this kind of job cannot stop) or 404.
  `/job` and `/jobs` now carry `cancellable: true|false` per job.
  - `edit-export` is cancellable: the export loop checks a `CancelToken` between rows,
    returns `Interrupted`, and the handler deletes the partial file — a half-written
    export that looks complete is worse than none.
  - `parquet` is not: it is one DuckDB statement, and offering a button that does nothing
    is worse than offering none. The 409 says exactly that.
  - Tests cover the wiring (a pre-cancelled token stops the export at the first row), the
    registry (a cancellable worker stops and reports `cancelled`, a plain job refuses),
    and the HTTP outcomes (404 for unknown, 200 for finished, `cancellable:false` in the
    listing).

## [3.22.0] — 2026-11-11

### Added

- **Graceful shutdown.** `SIGINT`/`SIGTERM` stop the accept loop, let in-flight requests
  finish and give running jobs up to five seconds, then exit 0. A test sends SIGTERM to a
  live server, waits for it to leave on its own and asserts the port no longer accepts —
  "graceful" is a behaviour, so it is asserted rather than described.
- **Request limits at the door**: query string 16 KB (`414`), request line 8 KB (`400`,
  which is what a body sent to a GET looks like), 64 header lines (`400`), 10 s read and
  30 s write timeouts. Before this, a client that opened a connection and said nothing
  held a thread until the OS gave up — a closed browser tab is exactly that client.
- **Symlink escape is refused**: `resolve_reason` canonicalises before the root check, so
  a link pointing outside resolves outside. A test now proves it with a real symlink, and
  also that a link staying inside the root still works.

## [3.21.0] — 2026-11-11

### Added

- **`GET /metrics`**: requests, `errors4xx`/`errors5xx`, `slowRequests` (≥1 s),
  `p50Ms`/`p95Ms` from fixed buckets (1 ms … 30 s), status-code distribution, per-route
  count/avg/max, latency buckets and job totals. Buckets rather than every sample stored
  forever: this server is meant to run unattended.
- **Every response carries `X-Response-Time`**, exposed to browsers via
  `Access-Control-Expose-Headers`. Recording happens in the response helpers — the same
  single place that emits CORS — so a new endpoint is measured without anyone
  remembering to measure it.
- **`/health` now separates liveness from readiness**: `ok` is the process, `ready` is
  "the served directory is readable". A test removes the root and asserts
  `ok:true, ready:false` — a process that is alive but cannot answer about files should
  not receive traffic.

### Notes

- `/metrics` counts itself after building the body, so its own request is one behind the
  caller's view. Documented rather than fudged: a metric that lies is worse than one that
  is late by one.

## [3.20.0] — 2026-11-11

### Added

- **The API is reachable from a browser without a proxy.** One place now emits the CORS
  answer — `Access-Control-Allow-Methods: GET, POST, HEAD, OPTIONS` (POST was missing,
  which broke every cross-origin write), `Max-Age: 600`, and the origin is exposed so a
  client can read `ETag`. `OPTIONS` answers a real preflight with 204.
- **`HEAD`** returns the headers a `GET` would return and no body.
- **`ETag` + `If-None-Match` → 304.** Every 200 JSON response carries a content hash; a
  matching validator gets 304 with no body, so paging through an unchanged window
  re-uses what the client already has. Errors are not taggable.
- **`GET /spec`**: version, reads, writes, `pathRequired`, caches, limits, `params` and
  `fields` per route, and the status-code meanings. It is generated from the server's own
  tables (`SPEC_PARAMS` / `SPEC_FIELDS`), and a test compares the listed fields with the
  live keys of `/rows` and `/cell` — a field that moves fails a test, not a client.

### Changed

- **Every window now has the same shape.** A plain page used to omit `totalExact`,
  `hasMore`, `edited` and `operations` while filtered and edited pages carried them;
  a client had to branch on the shape of a response. All pages answer the same keys now
  (filtered pages still omit `rowKeys`, and say why: their rows are not `start+i`).

## [3.19.0] — 2026-11-11

### Verified

- Cold-start evidence on a fresh copy of the 2.5 GB / 782-column file, with no cache and
  no index sidecar: `/rows?where=predict_status='success'&count=10` **0.30 s** on the
  first request and 0.17 s on the second; the directory afterwards still holds only
  `real.csv`. The response keeps `count`/`requested`/`limit`, `total` with
  `totalExact:false`, and still omits `rowKeys` on a filtered page.
- `docs/ACCEPTANCE.md` records the before/after table and names the three causes
  (type inference, dialect sniffing, a 64-row sample building a row index).

## [3.18.0] — 2026-11-11

### Fixed

- **A 64-row schema sample built a 2.5 GB row index.** `Sheet::schema` reads through
  `window`, which needs the sparse index, so the first SQL request on a fresh copy paid a
  full scan to create `t.csv.idx` — for twelve bytes of "is this a number?". The sampler
  used by the SQL layer now reads the header and the first 64 records directly
  (`sheet::schema_sample`, no journal, no index), and the cold filtered first page on the
  2.5 GB file went from **1.01 s to 0.22 s**, with no sidecar written.

### Changed

- `Sheet::schema` keeps its behaviour (edit-aware columns) but shares one classifier with
  the sampler, so "what kind is this column" has one implementation, not two.

## [3.17.0] — 2026-11-11

### Performance

- **Cold filtered first page on the 2.5 GB / 782-column file: 8–9 s → 0.22 s.**
  Declaring the column types was only half the fix. `read_csv` still ran its dialect
  sniffer — quote/escape/newline candidates over a large sample — before returning a
  row. With `auto_detect=false`, an explicit `quote='"'`/`escape='"'` and the declared
  columns, `SELECT … LIMIT 11` answers from the first block.

  | cold, no cache | before | after |
  |---|---|---|
  | dense filter (`predict_status='success'`) | 8–9 s | **0.22 s** |
  | match in the middle (row 300 000) | 15 s (cache path) | 1.48 s |
  | no match (full scan) | 15 s | 1.36 s |
  | ordered first page | 15.5 s | 1.36 s |
  | `count(*)` | 8.6 s | 0.71 s |

  A serial scan was tried first and rejected on evidence: it answers a dense filter in
  0.19 s but takes 8.5 s on a full scan, so parallel stays the default and serial stays
  the fallback for quoted-newline files.

### Verified

- New tests cover the dialect cases that `auto_detect=false` stops detecting: CRLF
  endings, quoted commas, a newline inside quotes (parallel reader refuses it — the
  ladder falls back), ragged rows, and a match in the last row of the file. Numbers in
  `docs/CAPABILITIES.md`; API.md gained a "what a first page costs" table by case.

## [3.16.0] — 2026-11-11

### Added

- **A columnar cache that makes a filtered page cost 0.26 s instead of 1.4 s** — and
  `0.34 s` for an ordered page, against `15.5 s` before this phase started.
  `POST /parquet?path=…` builds `t.csv.parquet` as a job (11 s for 2.5 GB, 427 MB with
  zstd), `/sheet` reports `parquet: {fresh, bytes}`, `/capabilities` advertises
  `caches: ["parquet"]`, and `csvx cache FILE --build/--drop/--status` does the
  same from the CLI.
  - Queries switch to the parquet view automatically once it exists, and go back to the
    CSV reader the moment the sheet changes — a cache that outlives its source would
    answer with yesterday's file.
  - **The cache cannot change the answer**: a test compares the same query with and
    without it (columns, rows, order) and asserts equality, because "faster" must not
    mean "different".
  - Typed columns first (numbers DOUBLE), auto-detected reader as the fallback, because
    a demo fixture with five ragged rows proved the strict reader rejects declared
    columns on files that `/query` already reads through auto-detection. The build tries
    both rather than failing on a file the rest of the tool handles.

### Docs

- API.md, CAPABILITIES.md and GETTING_STARTED.md document the cache with its size,
  build time, staleness rule and the CLI. `regress.sh` runs the four cache commands;
  `backend-smoke.sh` grew to 50 checks including the build job and a filtered page.

## [3.15.0] — 2026-11-11

### Performance

- **The first page of a filter went from 8–9 s to 1.4 s on the 2.5 GB / 782-column file,
  and the first page of an order from 15.5 s to 2.7 s.** Two separate causes, both fixed:

  1. **`read_csv_auto` was inferring 782 column types by reading the file.** `SELECT * FROM
     t LIMIT 1` took 7.3 seconds — almost the whole 2.5 GB read once for sniffing before
     the first row existed. The view now declares its types from a 64-row sample of our
     own reader, so the engine reads one block and answers. Numbers are declared DOUBLE
     (text comparison would order `10` before `9`), everything else stays VARCHAR.
     A sample can mislabel a column, so the auto-detected view remains as the fallback;
     a test builds exactly that case (80 numeric rows then text) and asserts the read
     still returns every row.
  2. **An ordered first page sorted 684 917 wide rows and collected 200 000 of them
     before answering.** `LIMIT`/`OFFSET` now go to the engine, so a page costs the rows
     it shows plus one.

- Column sampling is remembered per (path, size, mtime) instead of happening per request.

- `/query?sql=count(*)` on the same file: 8.6 s → 2.0 s, same reason as (1).

### Changed

- Filtered and ordered pages now name the parameter that failed (`where` vs `order`) in
  their 400/504 responses, and an unknown `order=` column is still reported with the
  friendly `cannot order by 'x': available columns: …` message rather than as SQL text.

## [3.14.0] — 2026-11-10

### Docs

- **`HANDOVER.md` and `ACCEPTANCE.md` brought to 3.13.0.** The handover now opens with
  "a CLI and an HTTP backend, no GUI", states the framework's three questions with the
  three layers that answer each, and lists the two limits this phase added (a 30-second
  query ceiling and what concurrency does and does not promise). The acceptance map
  gained a "framework's own contract" table: one table row per promise, each pointing at
  the test that holds it.
- Final numbers recorded: 494 workspace tests (275 core library tests with the `sql`
  feature), smoke 43, full command sweep 39, backend 44.

## [3.13.0] — 2026-11-10

### Fixed

- **`docs/INTEGRATION.md` told readers to run `serve --port 8787`, and `--port` did not
  exist.** The guide's first command failed with `unknown option: --port`. `serve` takes
  it now as a shorthand for `--addr 127.0.0.1:<port>` (`--port abc` says
  `--port takes a number, not "abc"`), and both forms are in `serve --help`.

### Added

- **Two tests that run the documentation check backwards.** The existing pair only proved
  "code is documented"; nothing proved "documentation is true". Now:
  - `every_documented_flag_exists` — every `--flag` any doc tells a reader to type is a
    flag the CLI accepts (cargo/git flags in examples are excluded, the changelog is
    excluded because history legitimately names removed flags, markdown rules and
    `--a/--b` prose are not flags).
  - `every_documented_route_is_served` — every `/route` a guide names is in
    `READ_ROUTES`/`WRITE_ROUTES`.
  The first one immediately found the `--port` lie above.

## [3.12.0] — 2026-11-10

### Added

- **A status-code contract, asserted case by case.** Seventeen failure kinds in one
  table test (`failure_kinds_have_one_status_code_each`): 400 for anything wrong with the
  request (path, column, SQL, parameter), 404 for a legal request whose target is not
  there (unknown endpoint, a row past the end), 405 for the wrong method, 409 for a
  compare-and-set that lost, 422 for an unreadable file, 504 for an interrupted query.
  - The codes were already consistent when probed endpoint by endpoint; the test is what
  makes "already consistent" stay true, and it is what a client can rely on when it
    switches on the status rather than parsing messages.
  - `docs/API.md` opens with the table; INTEGRATION.md points at the test.

## [3.11.0] — 2026-11-10

### Added

- **`stamp` on `/sheet` and `/journal`**: `ops:bytes:mtime` — a change detector, so a
  front-end can answer "is the screen I rendered still current?" with one comparison
  instead of a transaction. Pristine sheets answer `0:0:0`; the first write moves it, and
  both endpoints report the same string.
  - It is deliberately named a stamp, not a version: it detects change, it does not order
    edits. Correctness decisions use `expect=`; freshness decisions use the stamp.
- `docs/INTEGRATION.md` gained "并发与一致性": what the backend already handles (locked
  journal writes, 409 on a stale `expect`, lock-free reads that never show half an
  operation, stale-lock recovery after ten seconds) and what the front-end must do (re-read
  after writing, treat two edits as two operations rather than a transaction, and
  understand that `expect=` guards the cell's value *now*, not what you saw earlier).

## [3.10.0] — 2026-11-10

### Verified

Full backend regression on the real 2.5 GB / 782-column file, through HTTP, on this
version: `/capabilities` 1.5 ms, `/columns?name=` 2.8 ms, `/columns` (all 782) 0.67 s,
`/rows` at row 680 000 **2.4 ms** (0.9 ms for three columns), `/rows?keys=` 1.0 ms,
`/cell` 1.6 ms, `/journal` 0.4 ms, `/query` 8.6 s (one scan, now under the 30 s
interrupt), a compare-and-set write that succeeded and the same request refused **409**
afterwards with `actual: "checked"`, and an export job that finished in 40 s and kept the
input's BOM. The original file's sha256 is unchanged. Numbers replaced in
`docs/CAPABILITIES.md`.

## [3.9.0] — 2026-11-10

### Docs

- **`docs/ACCEPTANCE.md`**: every claim in the objective mapped to something that can be
  executed — the library function, the CLI command, the HTTP endpoint, the test that
  holds it, and the measured number where there is one. The four scripts at the bottom
  are the reproduction recipe.
- **`docs/API.md` has response-field tables** for `/rows`, `/columns`, `/cell` and
  `/journal`: what each field means, including the ones that are easy to misread
  (`count` vs `requested` vs `limit`, `total` vs `totalExact`, `start` is 0-based input
  while `rowNumbers` are 1-based output, `rowKeys` absent on filtered pages).
- **README got a literal quickstart**: four `curl` lines against `127.0.0.1:8080`, each
  with the sentence describing what it answers. They were run verbatim before committing;
  the first draft used `…/rows` shorthand, which is not a command anyone can paste.

## [3.8.0] — 2026-11-10

### Added

- **Queries are bounded by a wall clock and actually stopped.** `/query` and the
  filtered `/rows` path now run with a 30-second limit, enforced by DuckDB's interrupt
  handle: a watchdog thread fires `interrupt()`, the engine aborts the scan, and the
  client gets `504 {"error":"the query was interrupted","parameter":"sql","hint":…}`
  instead of a request that hangs until the socket gives up. `/capabilities` publishes
  `limits.querySeconds`.
  - Enforced rather than declared: a test runs a query with a zero timeout and asserts
    the result is `TimedOut` with the hint attached. A machine fast enough to beat the
    watchdog is reported, not silently passed.
  - A 2.5 GB scan takes about eight seconds here, so thirty seconds is generous for an
    interactive request and still finite.

## [3.7.0] — 2026-11-10

### Added

- **`/capabilities` says which reads need `?path=`** (`pathRequired`), and publishes
  `limits.queryRows`. A client can discover "this endpoint wants a sheet" instead of
  learning it from a 400.
- The 2000-row window cap is now pinned by tests and by `scripts/backend-smoke.sh`:
  `count`, `requested` and `limit` must all be right at once (`count` 2000,
  `requested` 5000, `limit` 2000). It was already implemented; nothing was checking that
  a client could see it.
- `backend-smoke.sh` gained write-side error cases: an unknown `action` and a missing
  `columns=` name. 43 checks.

### Docs

- API.md documents the cap as three fields, INTEGRATION.md's "do not assume" table warns
  that `count` is never above 2000 and points at the capability that states the number.

## [3.6.0] — 2026-11-10

### Added

- **Truncated listings say how many there are.** `/files?limit=1` reported
  `truncated: true` and nothing else — a client knew something was hidden but not how
  much. It now carries `total` (every match, counted after the cut), and `/book?max=1`
  carries `hidden` and `total` alongside `truncated`. `Listings::total` counts what the
  limit hid rather than what the loop happened to look at.
- **`docs/INTEGRATION.md` has an error cheatsheet**: the five status codes, their shapes,
  and what a front-end should do for each — 400 means "read the hint and fix the
  parameter", 409 means "re-read the cell and ask the user", 422 means "this file cannot
  be read, do not retry". `detail` is flagged as developer-facing text that should never
  be shown to a user verbatim.

## [3.5.0] — 2026-11-10

### Fixed

- **`/book?root=<subdir>` scanned the served root instead of the subdirectory**, and a
  root that did not exist fell back to the root silently — a client asking for one
  directory was handed another's sheets and had no way to tell. The path is validated
  (`400` naming `root`) and the scan uses it.
- **`/download` answered one message for four path mistakes.** It was still on the older
  resolver while the rest of the server used `resolve_reason`, so "file not found",
  "that is a directory" and "outside the served root" all read
  `path missing or outside root`. Each now gets its own message, which is what the
  documentation already claimed.

### Changed

- **One route list instead of three.** `READ_ROUTES` and `WRITE_ROUTES` feed the
  dispatcher's 404 fallback and the `/capabilities` response, and a test walks the
  dispatcher's own arms to prove nothing is served without being advertised. The old
  copies had already drifted: `/schema`, `/cell` and `/journal` were served but absent
  from `/capabilities` and from the 404 list.
- A second test compares `READ_ROUTES` with `docs/API.md`'s endpoint index.

## [3.4.0] — 2026-11-10

### Added

- **`pastEnd` on every window**: an empty page now says whether it is empty because the
  offset went past the file or because there is nothing to page. A client renders
  "no more rows" for one and "empty file" for the other; `total` alone made it guess.
  An empty file (`total: 0`) is explicitly *not* `pastEnd`.

### Fixed

- **The timing assertion in `a_repeated_filtered_page_does_not_rescan` was measuring the
  machine under load.** Its bound was `max(cold, 50 ms)`; in a full gate run every other
  test server competes for CPU, so a 30 ms cold page and a 60 ms warm one failed an
  assertion about caching. The bound is now `cold × 3 + 200 ms` — a rescan costs about a
  cold run, so this still fails loudly for the regression it guards, and passes when the
  box is busy. This is the one unexplained failure from the previous round.

### Docs

- README now says `schema --diff` exits 1 when the column sets differ, and API/INTEGRATION
  document `pastEnd` next to the fields that page numbers depend on.

## [3.3.0] — 2026-11-10

### Added

- **`/query` reports the limit it applied**: `"limit":10000,"requestedLimit":50000` when
  the ceiling clamps the request. Truncation that hides its own ceiling is how a client
  ends up believing it saw every row.
- **`docs/API.md` has an endpoint index**: method, path and purpose in one table, so
  "is this GET or POST?" stops being a guess. `/edit` appears twice on purpose — GET is
  the read-only state, POST is the write.
- `scripts/backend-smoke.sh` grew keys and summary cases: `keys=` resolves to row numbers
  and honours `columns=`, and `/sheet` carries `operations`, `lastOperation` and
  `capabilities`. 38 checks now.

### Docs

- **README says which command reads columns and which rewrites them.** `schema` reads
  metadata; `columns` keeps/drops/renames and writes a file. Two commands with
  near-identical names doing opposite things is a trap, so the table says so with a
  warning marker instead of relying on the reader's attention.

## [3.2.0] — 2026-11-10

### Changed

- **`/sheet` no longer repeats the operation list.** Two endpoints returned the same
  facts with different shapes, which is how a client ends up depending on one and the
  other drifts. `/sheet` gives the summary — `operations` (count), `lastOperation`,
  capabilities, layout, `indexed` — and `/journal` gives the stack, the redo side and
  the damage counts.

### Fixed

- **`/rows?keys=` ignored `columns=` and `start-col`.** Asking for a kept selection and
  then narrowing it returned every column, because the keys endpoint had its own
  response builder without the shared projection helpers. It uses them now, so a
  selection can be projected and sliced like any other window.

### Docs

- INTEGRATION.md gained "表的概况" — what `/sheet` answers before the first row is drawn
  (pending operations, whether random access is paid for) — and API.md now states which
  endpoint owns which fact.

## [3.1.0] — 2026-11-10

### Fixed

- **`/rows?where=` reported a fake total.** It returned the cache prefix
  (`total: 200000, hasMore: true`) for a filter that matches 684 917 rows — a front-end
  would render "200000 rows" and be wrong by a factor of three. Filtered pages now carry
  `totalExact`: when the page reached the end of the result, `total` is the count; when
  it did not, `total` is a lower bound and says so.
- **A filtered page was handed `rowKeys` it could not honour.** The keys were computed as
  `b:<start+i>`, which is only correct for an unfiltered window; for a filtered page the
  rows come from the engine and row 98 of the file is not row 1 of the result. Filtered
  pages no longer include keys, and the docs say where keys are meaningful.
- **`OFFSET` was a forbidden word.** `validate` matched forbidden keywords as substrings,
  so `SELECT * FROM t LIMIT 10 OFFSET 20` failed with "'set ' is not allowed in a query".
  Keywords are matched as whole words now; the table-function names that must catch
  `read_csv_auto(...)` stay prefix matches, because that is what they are.

### Performance

- A filter without an order now asks DuckDB for `LIMIT count+1 OFFSET start` instead of
  collecting the whole result: on the 2.5 GB / 782-column file the first page of a
  selective filter went from **15 s to 9 s**, and a page that matches everything from
  19 s to 8 s. It is still a scan — a filter over 2.5 GB costs a scan — but it no longer
  materialises 200 000 rows to show ten.

### Build

- **`scripts/dev.sh gate` now runs core's own tests with `--features sql`.** `sql` is an
  optional feature that only the CLI enables, so `cargo test --workspace` built core
  without it and skipped the module's tests entirely — including the read-only guard
  above. That is how the `OFFSET` regression survived until the guard was exercised.

## [3.0.1] — 2026-11-10

### Verified

- **The backend measured on the real 2.5 GB / 782-column file**, through HTTP, release:
  - `GET /columns?name=model_prob` **3.3 ms**, 47 bytes — the column-existence question.
  - `GET /columns` (all 782) 0.66 s, 107 KB.
  - `GET /rows` at row 680 000: **2.0 ms** for the full 782-column window (87 KB), **0.9 ms**
    and 651 bytes with `&columns=cust_no,model_prob,predict_status`.
  - `GET /cell` 0.8 ms, `GET /journal` 0.3 ms.
  - `GET /query?sql=SELECT count(*) FROM t` 7.9 s — one full scan, as expected.
  - `POST /edit?action=export` returns a job (202) and finishes in 33 s; the file it
    writes keeps the input's BOM.
  - The original file's sha256 is unchanged after all of it.
- `scripts/backend-smoke.sh` grew the async-export path: queue the job, poll `GET /job`,
  assert the file exists and starts with the header. 32 checks now.

## [3.0.0] — 2026-11-10

### Added

- **`scripts/backend-smoke.sh`**: start a real server, make real requests, assert
  27 things a front-end depends on — the capability list, column metadata and existence,
  the window contract (`count`/`requested`/`limit`/`rowKeys`), column projection,
  horizontal slicing, single-cell read, the three states of `expect=` (written, 409,
  409 again once the value moved), the journal stack, and five error codes. Cargo tests
  call handlers; this calls the thing a client talks to.

### Fixed

- **SQL and ordering mistakes were answering with raw engine text.** `?sql=SELECT nope`
  returned `{"error":"Binder Error: … DESCRIBE SELECT …"}` — the client's own mistake,
  reported as if the file were broken. Engine failures are now classified as input
  errors: `400 {"error":"the query could not be run","parameter":"sql","hint":…,
  "detail":"Binder Error: …"}`, and an order on a column that does not exist is caught
  before the engine sees it (`cannot order by 'amount': … available columns: id, city`).

## [2.9.0] — 2026-11-10

### Fixed

- **`order=amount:desc` on `/rows` reached DuckDB verbatim**, so the client got
  `Parser Error: syntax error at or near ":" … DESCRIBE SELECT …` — a message about SQL
  it never wrote. The API accepted only the older `order=amount&desc=1` shape while the
  CLI and `edit export` accepted `amount:desc`; two callers had their own copy of the
  clause builder and only one knew the shorthand.
  - One `order_clause` now serves both, accepting `col`, `col:asc`, `col:desc` and
    comma-separated lists, and refusing anything that is not a column name with a 400
    naming `order`. The injection attempt `order=amount;DROP` is rejected before it
    reaches the engine and no longer produces a raw parser error.

### Verified

- The three window modifiers compose: `order=amount:desc` + `columns=id,amount,note`
  + `start-col=1&count-cols=1` returns exactly one column and the two highest amounts.
  A test pins that combination, and another pins `where=` with `columns=`.

## [2.8.0] — 2026-11-10

### Added

- **`GET /journal`**: the undo stack, described. Each operation carries the same
  1-based sentence the CLI prints (`set row 1 'city' = 'X'`), plus `count`,
  `redo`/`redoCount`, `canUndo`/`canRedo`, and a `damaged` block that is non-zero only
  when a journal record could not be read. A toolbar can label its buttons from this
  instead of re-deriving or remembering anything.
- `Journal::redo_ops()` so callers can read the redo stack without popping it.

### Fixed

- **`POST /edit?action=insert-column` worked but was not advertised.** The action
  existed in the handler and was missing from `EDIT_ACTIONS`, so the "unknown action"
  hint — the list a client reads when it guesses wrong — omitted it (and
  `split-column`, and `fill`). The constant now carries all fourteen, and a test posts a
  bogus action and asserts every real one appears in the hint.

## [2.7.0] — 2026-11-10

### Added

- **`schema --diff OTHER`**: compare two files' column sets — what only the first has
  (removed), what only the second has (added), and what exists in both but at a different
  position. `--json` for machines, exit 1 when anything differs, so a pipeline can gate
  on "did the upstream table change shape?" without parsing text.
- **`/columns?kind=` and `?limit=`**: list only columns of one kind (a 400 listing the
  five when the name is wrong), or truncate for rendering — `limit` still reports
  `totalColumns` and `shown`, because a truncated list that hides its own truncation is
  worse than a long one.

### Fixed

- **`start-col`/`count-cols` were ignored on filtered windows.** `/rows?where=…` went
  through the SQL path, which had its own copy of the projection block; the horizontal
  slice had been wired into the other copy. Both now share the same two helpers, and a
  test asserts the combination (`where` + `start-col` + `count-cols`).

### Docs

- `docs/INTEGRATION.md` gained "一个完整的编辑循环": the six calls a front-end actually
  makes, with the three rules that follow from this backend's design — re-read instead
  of predicting, remember keys instead of positions, and treat 409 as "re-read and ask"
  rather than a retry.

## [2.6.0] — 2026-11-10

### Added

- **`GET /rows?keys=b:12,v:3`** — the inverse of `rowKeys`. A front-end keeps a
  selection by key; after an insert moved the table it asks where the key is now and
  gets `rowNumbers`, in the order asked for. Keys that are gone (a deleted row) come
  back in `missing` rather than being silently dropped. Up to 200 keys, and an
  unreadable key is a 400 naming `keys`.
  - `RowMap::logical_of` does the lookup in time proportional to the structural
    operations, not to the rows: an insert or delete is what changes a position.
- Two tests: keys resolve before any edit and report stale ones; after an insert at the
  top, `b:0` still points at the same *row*, with row number 2 instead of 1.
- `docs/INTEGRATION.md` gained the "where did my selection go" call, and API.md the
  endpoint plus a table of what the two key shapes mean.

## [2.5.0] — 2026-11-10

### Added

- **`schema --require a,b,c`**: assert a whole column set at once — exit 0 when all are
  there, exit 1 naming the missing ones with the same "did you mean" candidates every
  other lookup uses. A pipeline can check its inputs before doing any work.
- **`distinct` on every column**: `/columns` and `schema` now report how many distinct
  values the sample held (capped at 64, flagged `distinctCapped`). That is the question
  a front-end actually asks — "is a dropdown plausible here, or is this free text?" —
  and sampling answers it in the same pass that already computes the kind.
- **`docs/INTEGRATION.md`**: the one page a front-end developer needs — start the
  server, get the columns, page rows, horizontal scroll, read one cell, write one cell
  conditionally, and understand row keys. It ends with a "do not assume" table (rows
  can be shorter than `count`, `kind` is a sample, writes can 409, the cap is 2000, the
  original file is never touched) and the error shapes.
  - A test checks the guide against `serve.rs`: every route it names is served and every
    parameter it promises is read somewhere. Documentation that cannot drift.

## [2.4.0] — 2026-11-10

Framework, round two: the reads a front-end needs to render a huge wide sheet.

### Added

- **`GET /cell?row=&col=`** — one cell by name, with its stable row key. The read half
  of `expect=`: read the value, decide, then write conditionally. `404` past the end
  (and says the total), `400` naming `col` when it does not exist.
- **`rowKeys` on every window**: `b:<index>` for a base row, `v:<id>` for an inserted
  one, parallel to `rows`. A selection that survives an insert is a selection by key,
  never by position — this is the identity edits are stored against, finally exposed.
  (`Window` carries them now, so CLI and API share the same keys by construction.)
- **`start-col=` / `count-cols=` on `/rows`**: horizontal paging. A 782-column sheet
  answers a screen-sized slice (`count-cols=5`) without building the other 777.
- **`/schema` as an alias of `/columns`**, for callers that think in schema.

### Docs

- `kind` is documented as what it is: a sample of the first `--sample` rows to choose an
  editor, **not** a promise about row 11 onward. Full-column truth is `analyze`; an
  assertion is `validate`.
- A test now checks that every route the code serves appears in `docs/API.md` — it
  immediately caught `/cell` and `/schema` being undocumented.

## [2.3.0] — 2026-11-10

Framework round: discovery, projection, and the one race that could lose an edit.

### Added

- **`GET /capabilities`**: reads, writes, edit actions, limits and write guards in one
  response, generated from the same constants the handlers use. A front-end no longer
  learns the surface from a document that can drift.
- **`/rows?columns=a,b,c`**: a window can ask for three columns of a 782-column sheet.
  Same rows, same numbers, **87 KB → 529 bytes**. Unknown names get a 400 naming
  `columns` and listing what exists; `frozenColumns` narrows with the selection.

### Fixed

- **Two concurrent edits could lose one another.** `Journal::save` writes a temp file
  and renames it, so two processes that both loaded the old journal and both saved
  ended with the last rename winning — one edit gone, no error anywhere. All writes
  (set, insert, delete, paste, rename, undo, redo) now go through
  `Journal::update_locked`: a lock file, a fresh read inside it, then the save. Ten
  concurrent writers leave ten operations, and each lands on its own row. A lock left
  by a killed process is presumed stale after ten seconds so a crash cannot wedge a file.
  - The lock is not a substitute for `expect=`: the lock keeps both edits, `expect`
  decides whether yours should happen.

## [2.2.0] — 2026-11-10

The project is now a **CLI and a backend**, not a GUI application. The framework is what
matters; a front-end is someone else's job.

### Removed

- **The GUI crate is gone** (`crates/gui`, egui/eframe/rfd, ~950 lines and every Linux
  font/GTK dependency). Keeping a frozen front-end compiling for its own sake was work
  that produced nothing the user asked for. The workspace is now `core` (engine) and
  `cli` (commands + HTTP backend). CI and the release workflow build `csvx`.

### Added — the three questions a front-end asks

- **`sheet::schema()` and `sheet::column_index()`** in the library: the full column list
  with 1-based index, sampled kind (`number`/`date`/`text`/`empty`/`mixed`) and blank
  count, plus "does this column exist, and where".
- **`csvx schema FILE [--has COL] [--json] [--sample N]`**: the same two answers
  on the command line. `--has` exits 0 when the column exists and 1 when it does not,
  so a script can branch on it.
- **`GET /columns`**: the column list, never truncated (782 columns return 782 entries),
  and `?name=` for a single existence answer — 9 ms on a 2.5 GB / 782-column file,
  instead of downloading the list to find one missing name.
- **Compare-and-set writes**: `edit set --expect VALUE` and
  `POST /edit?action=set&expect=…`. If the cell is not holding the expected value the
  write is refused, `actual` says what it holds, and the API answers **409** rather than
  a silent overwrite. A front-end editing a window it fetched a moment ago no longer
  races the writer.

### Fixed

- **A wide CSV export ran at 62 MB/s** because every row was materialised as 782 owned
  strings before being written. Rows the journal does not touch are now copied byte for
  byte: a 2.5 GB / 782-column export went from **40.7 s to 2.2 s**, and the output is
  byte-identical to the input (`sha256` matches). The fast path is off when the file's
  quoting style differs from what export writes (a backslash file is still normalised
  to RFC 4180, as it always was).

## [2.1.0] — 2026-11-10

First run on **real business data** — a 2.5 GB, 685 k-row, 782-column result file:

### Fixed

- **A UTF-8 BOM was silently dropped.** The original starts with `EF BB BF`; every
  output started with `base_time`. Excel reads a BOM-less UTF-8 file as the system code
  page, so a file with Chinese text would have opened as mojibake — the exact thing
  "works like Excel" means. `export`, `filter -o` and `columns` now keep the input's BOM,
  and a test asserts both directions: keep it when present, never invent one.
- **A 782-column file flooded the terminal.** `preview` printed all 782 columns (38 KB
  for one page), burying the three columns anyone reads. Terminal tables now show
  twenty and say `(+762 more column(s) not shown ...)`. `preview` and `view` gained
  `--columns` so the rest is reachable — the note names an option that exists, which is
  the lesson from 2.0.2 applied before it could bite again.
- **`validate` rebuilt a full-width probe for every row, for every compare rule** — 782
  empty `String`s × 2 rules × 685 k rows. One probe per rule, reused: 72 s → 61 s on the
  real file.

### Measured on the real file

`index` 0.31 s / 15.9 MB / 5.4 KB of index for a 2.5 GB file; any offset pages in 12 ms;
`groupby` 14.6 s at 9.0 MB; `filter` 13.5 s writing 1.28 GB. `export` is 40.7 s (62 MB/s)
because the pipeline materialises all 782 cells per row as owned `String`s — the
structural cost of the row API, written down in CAPABILITIES with the improvement it
would need.

## [2.0.2] — 2026-11-10

### Fixed

- **`layout --freeze-first` said "Freeze these leading columns"** and took a number,
  so `--freeze-first "id,cust_no"` failed with `bad --freeze-first` — the help implied a
  list and the error said nothing about what was wanted. It now reads "Freeze the first
  N columns (a number, e.g. 2)" and the error is
  `--freeze-first takes a count, e.g. 2, not "id,cust_no"`.
  - Found by walking the roadmap items one last time: pivot, conditional formatting,
    chart data, layout metadata and filtered editing all worked, and then `layout`
    refused an argument its own help had suggested.

## [2.0.1] — 2026-11-10

### Docs

- **`docs/HANDOVER.md`**: the state of the project after eighty rounds — what it is
  in one sentence, coverage against Excel/WPS operation by operation, where the evidence
  for each of the three promises lives, the measured performance table, six known limits
  stated rather than implied, how to continue (which doc to read before touching which
  part), and the six lessons the rounds actually taught.
- A test checks that every command the handover names exists in `--help`, because it is
  the first file a new maintainer reads and a stale command name there is more
  expensive than the same mistake in a comment.

## [2.0.0] — 2026-11-10

79 rounds of work, and the reason this is a major version rather than 1.15.1: three
things that were promises became tested facts.

### The three promises, each with the test that holds it

| Promise | Test |
|---|---|
| The input file is never modified | `no_command_modifies_the_input_file` — hash, size and mtime across every writing command, including one that fails on purpose |
| Memory does not grow with rows | measured per command (77 MB fixture: 8-25 MB except the buffers), plus an export of 5 M rows with a formula column at 17.3 MB peak |
| Anything that could mislead says so | error contract, `truncated` flags, `#VALUE!` cells, journal-damage warnings, four distinct path messages, and `troubleshooting_quotes_are_real` |

### What is actually here

Thirty commands. Twenty-one read endpoints and eleven write actions. Editing without
rewriting the file: set, fill, insert, delete (by row, by condition, by validation rule,
by content), replace, split-column, formulas with `lag`/`running_total`/cross-sheet
`ref`, undo and redo. Analysis: filter, search (with patterns), sort, dedupe, groupby,
pivot, chart, profile, validate, conditional formatting, layout metadata. Delivery:
split, merge, verify, extract, partition, concat, lookup, export with filter/columns/
order, and a local HTTP API with a built-in viewer.

### What the last twenty rounds were

Not features. They were the price of the first sixty: combination semantics (renames,
split pieces, fills, deletes) that only broke when two correct things met; documentation
that had drifted from the code, including an API reference that had been the changelog
for twenty-nine versions; and performance claims that turned out to be false on one of
their two paths — deep paging took 5 s at offset 3.9 M, and `verify` held both files in
memory.

Every one of those now has a test built from the source of truth rather than from a
list someone maintains: command names from `main.rs`, flags from the parser, routes from
`serve.rs`, modules from the filesystem, messages from the binary.

### Known limits, stated rather than implied

- `running_total` is `#VALUE!` in a windowed view (it needs every row before the start).
- A1 references that need another row report `#VALUE!`; only same-row references work.
- `sort` / `concat` / `sql` do not accept `--quotes`.
- `analyze` / `stats` / `search` / `sort` / `dedupe` / `concat` / `columns` / `split` /
  `partition` / `lookup` read the file, not the edited sheet (`groupby` / `pivot` /
  `chart` take `--apply-edits`).
- `dedupe` memory grows with distinct keys, `sort`/`partition` with `--buf-mb`.
- The built-in page has not been exercised in a real browser; Windows is untested.

## [1.15.0] — 2026-11-10

### Removed

- **Dead public API**: ten names that nothing called, found by scanning every `.rs` file
  for identifiers with a single occurrence — `sheet::count_data_rows`, `sheet::row_id`,
  `csv_reader::count_records`, `csv_reader::next_record_string`,
  `encoding::parse_name_exact`, `preview::parse_plain_line`, `aggregate::render_parts`,
  `viewer::with_index`, plus the imports they had been keeping alive.
  - Two carried comments describing features that do not exist: `parse_plain_line` was
    "kept for API compatibility" (nothing kept it) and `with_index` was "e.g. one built
    on a worker thread" (nothing builds on a worker thread). A comment promising a caller
    who is not there survives every refactor as false evidence.
  - Dependencies were checked the same way: seventeen in core, all used.

## [1.14.0] — 2026-11-10

### Docs

- **Every performance number re-measured** on a 215 MB / 5 M-row file in release; the
  README table had grown a mixture of fixtures from different rounds. Index 0.08 s / 16 MB
  / 40 KB, jump 0.01 s, `analyze` 1.64 s / 17 MB, split ×8 0.41 s (0.23 s with four
  threads), merge 0.18 s, groupby 1.38 s / 9 MB.
  - **Export with a formula column over 5 M rows: 17.3 MB peak** — the flat-memory
    promise on the operation that matters most, now measured rather than assumed.
  - API on the same file: first page 66 ms including index build, later pages 6–7 ms
    (constant time at any offset), a filtered first page 296 ms and its second page 15 ms.

## [1.13.0] — 2026-11-10

### Docs

- **`docs/TROUBLESHOOTING.md`**: the messages the tool actually prints, grouped by
  symptom — typo'd column, formula naming something that does not exist, files that will
  not open, index refusals, shifted fields, missing edits, a vanished edit, the overwrite
  guard, every limit, 1-based rows, and "why is it slow".
  - Every quoted sentence is reproduced by a test (`troubleshooting_quotes_are_real`),
    including the two `hint:` lines and the journal-damage warning. A guide that has
    drifted teaches the reader to look for a sentence that no longer exists, so it is
    checked against the binary that ships with it rather than trusted.
  - The speed section separates the three kinds of slowness the engine really has:
    index building (once), filter materialisation (once per query), and the bounded
    buffers that `sort`/`duplicate`-removal hold on purpose.

## [1.12.0] — 2026-11-10

### Docs

- **`docs/ARCHITECTURE.md`**: the map that 74 rounds of work never wrote down — the on-disk
  format, the editing model with its two identity axes, the three `/rows` paths, the
  module map, where each invariant is enforced, and a "if you change X, also check Y"
  table whose rows are all backed by tests.
  - The subtle parts are the ones three bugs came from: derived values read the *base*
    text (split pieces), the *row identity* (fills), or the *finished row* (formulas).
    That distinction is now written where someone changing `workbook/rows.rs` will see it.
  - A test walks `crates/core/src` and fails when a module is missing from the map, which
    immediately caught the formula submodules (`functions`, `state`, `sheetref`) that
    the first draft had collapsed into `formula`.

## [1.11.0] — 2026-11-10

### Fixed

- **A journal cut off mid-write dropped its last operation without a word.** The records
  that could be read still applied, and the partial one was silently ignored — which
  reads as "my edit vanished". It now says
  `warning: 1 record(s) in t.csv.journal could not be read and were ignored`,
  naming the file, and separately for the redo sidecar.

### Verified

- Damage handling, one sidecar at a time: a truncated journal keeps the readable
  operations and reports the rest; a garbage or truncated `.idx` is rebuilt and the
  rebuilt index still points at the right rows; a nonsense `.view` falls back to
  defaults; nine failure modes leave no partial output. Append-only lines are why an
  interruption costs one operation rather than the whole history.

## [1.10.0] — 2026-11-10

### Verified

- **No command modifies the input file.** Hashed, sized and mtime-stamped before and
  after every writing command — filter, sort, groupby, validate, edit set, edit export,
  split — plus one that fails on purpose. All identical. The promise had been checked
  one command at a time; now a test checks the set.
- No failed command leaves an output file behind: bad column, bad order, bad partition
  key, bad split key — nine failure modes, zero leftovers.

### Added

- **A formula is checked when it is written, not when it is exported.** `edit formula
  --value "amonut*2"` now fails with "unknown column(s): amonut (available: id, city,
  amount)" instead of recording a step whose export fails later. A `ref()` pointing at a
  sheet that does not exist is refused with the same rule the export applies, plus a
  hint showing the expected shape.

## [1.9.0] — 2026-11-10

### Fixed

- **`verify` loaded both files into memory.** It was `std::fs::read(input)` and
  `std::fs::read(merged)` followed by `==` — 197 MB peak on a 77 MB file, and 20 GB on
  the 10 GB splits that `verify` exists to check. It now compares in 256 KB chunks:
  44 MB, flat in file size. A mismatch reports the byte offset and both lengths.

### Verified

- Every command's peak RSS measured in release on a 4 M-row / 77 MB file:
  `preview`/`search`/`chart`/`pivot`/`groupby` 8–9 MB, `filter`/`columns`/`validate`
  12–13 MB, `index` 16 MB, `analyze`/`stats` 23–25 MB, `split` 64 MB, `sort` 218 MB,
  `partition` 287 MB, `dedupe` 285 MB with 4 M distinct keys but 9 MB with 20.
  The doc now states the honest version of "memory does not grow with rows": it does
  not, **except** where a command must remember every row (`dedupe` remembers keys,
  `sort`/`partition` hold a buffer), and each of those has an explicit cap that errors
  instead of growing silently.

## [1.8.0] — 2026-11-10

### Fixed

- **A page at a deep offset cost a scan of everything before it.** `view --start 3900000`
  on a 77 MB file took 5.1 s and scaled with the offset (1.0 M → 1.5 s, 2.0 M → 2.7 s,
  3.9 M → 5.1 s): the unfiltered path looped forward from row zero to reach the offset
  instead of jumping to it. It now jumps: 0.31 s at every offset. A filter still scans,
  because that is what a filter means.
- **`edit export --columns … --order …` refused to order by a column it was told to
  drop.** "Sort by amount, deliver id and city" is a normal request; it now exports
  every column, sorts, and then drops the rest, and the summary reports the width
  actually delivered.
- **`layout --set`'s help example was wrong**: it said `width=20`, and the real syntax
  is `column:key=value` — the example failed if pasted.

### Added

- **`scripts/regress.sh`**: every command once, on a generated file, checking that
  each one answers. This round's sweep found the three problems above, two of which
  (`--order` on a dropped column, the `layout` example) had never been exercised by any
  test because they read like reasonable requests rather than edge cases.

## [1.7.0] — 2026-11-10

### Added

- **`edit delete --invalid RULE`**: remove the rows a validation rule rejects.
  `validate` found the bad rows; nothing could act on them, so the cleanup was a loop
  over `edit delete --row`. `edit delete --invalid "ts:date"` is one operation and one
  undo.
- **`groupby` / `pivot` / `chart --apply-edits`**: these read the file, so a formula
  column that lives only in the journal was invisible to them — "add a column, then
  group by it" needed a manual `edit export` in between. With the flag they materialise
  the sheet to a temporary file first, and the pending-edits warning is suppressed
  because the edits really are applied.

### Fixed

- **`validate` printed a progress line per page**, eleven of them for 20 000 rows that
  take under a second. Progress is now reported at ten-percent steps, and only above
  100 000 rows, where it is worth having.

All of this came out of walking the user journey end to end (analyse → validate →
clean → compute → summarise → deliver) and finding where a step needed a detour.
The journey is now a test, so the detours cannot come back unnoticed.
## [1.6.0] — 2026-11-10

### Fixed

- **The three `/rows` paths answered with different fields.** The plain window reported
  `edited` / `operations` / `frozenColumns` and no `hasMore`; the filtered and ordered
  windows reported `hasMore` and none of the context. A client reading pages needed two
  parsers depending on whether a filter was active.
  All three now report the same set, and the filtered ones say `edited` too, since they
  materialise a table that includes the journal.

### Verified

- **Read paths see edits.** `view --where`, `view --order` and `/rows?where=` /
  `/rows?order=` all use edited values: turning the only Puebla row into CDMX makes ten
  rows match instead of nine, and raising one `qty` to 999 puts it first when ordering
  descending. A formula column can be filtered on the same way a formatting rule can
  match it — the filter runs on the finished row in every path.

## [1.5.0] — 2026-11-10

### Fixed

- **A fill did not follow its rows.** Fill rows 6-8, then delete rows 1-3, and the values
  slid up to rows 3-5 — position semantics, while `set --row` uses identity semantics.
  Every other edit sticks to its row, so the fill was the odd one out.
  - Fills now resolve to row identities **when the edit is made**: a contiguous run of
    base rows is stored as a span (filling a million rows still costs one entry), and a
    range that spans inserted blocks keeps the identities it touched.
  - Found by probing the combination, not by reading the code: each mechanism was
    correct on its own.

## [1.4.0] — 2026-11-10

### Added

- **`edit delete --where`** (`POST /edit?action=delete-where&where=`): remove every row
  that matches, as **one** journal operation — one `edit undo` brings all of them back.
  ```
  edit delete t.csv --where "city=Puebla"
  ```
  - Matching is resolved **when the edit is made**, then stored as row ranges, so
    replaying the journal later needs no scanning and the journal stays a description
    of what happened rather than a program to re-run.
  - Ranges are applied bottom-up during replay; the top-down version deleted the wrong
    rows, which is the failure this tests first.
  - A condition that matches more than 1 000 separate stretches is refused: an
    alternating match would otherwise turn one edit into a million journal entries.

## [1.3.0] — 2026-11-10

### Added

- **`edit set --find`** (and `edit delete --find`): locate the row by its content instead
  of copying a number out of a viewer.
  ```
  edit set t.csv --find Puebla --col city --value "Puebla, MX"
  edit set t.csv --find CDMX --nth 2 --col qty --value 999
  ```
  `--find-col` narrows the search; `--col` is the column to write, not where to look
  (an early version confused the two, and a search restricted to the target column
  finds nothing whenever the needle is elsewhere). Not found exits 1 with the text it
  looked for.

### Fixed

- **Operation descriptions printed 0-based rows.** `edit set --row 1` reported
  "set row 0", and `insert`/`delete` said "at 0" — the journal's internal index leaking
  into text meant for people. Found while using `--find`, whose whole point is that the
  user does not have to hold a row number in their head.

## [1.2.0] — 2026-11-10

### Added

- **`edit export --columns` and `--order`**: select the columns to write, and the order
  to write them in, in the same pass. "Filter, reorder, drop columns, export" was four
  commands and two passes over the data; it is one command now.
  ```
  edit export t.csv --columns "id,amount" --order "amount:desc" --where "qty>50" -o out.csv
  ```
  - Column selection happens while the row is being assembled, so the sort that follows
    sees only the columns being written — not the whole row.
  - `--order` stages the export and reuses the existing streaming sort, so memory stays
    bounded; the staging file is removed on both the success and failure paths.
  - A column that is not in the sheet is refused with the list of ones that are, rather
    than silently producing a short row.

## [1.1.0] — 2026-11-10

### Added

- **`edit fill`**: one value across a row range — Excel's drag-fill.
  `edit fill --col qty --from-row 5 --to-row 10 --value 0`, or
  `POST /edit?action=fill&col=qty&from-row=5&to-row=10&value=0`.
  - Stored as **one journal operation holding a range**, not one per row: filling
    200 000 rows must not cost 200 000 entries, or the memory promise ("cost follows
    edits, not rows") would be a lie for the most common bulk edit there is.
  - A precise `set` always beats a bulk fill that covers it, whatever order they were
    made in — a deliberate rule, written down rather than left to interpretation.
  - The flag names are `--from-row` / `--to-row` because `--from` / `--to` already mean
    "old name" and "replacement text"; renaming a column and filling rows are different
    verbs and should not share a spelling.

## [1.0.2] — 2026-11-10

### Fixed (docs)

- **`docs/CAPABILITIES.md` documented a flag that does not exist**: it showed
  `merge -i part*.tar`, and `merge` takes the archive positionally and rejects `-i`
  with "unknown option". A reader following that line got an error before anything
  worked.
- The page also never mentioned `serve`, `demo` or `files` — three commands missing
  from the page that claims to list capabilities.

### Added

- A test checks both directions against the command list in `main.rs`: every command
  named here must exist, and every command must be named. The first version flagged
  `crates/core`, `lag(...)`, `where=…` as phantom commands, so the check now only
  accepts a cell that reads like `cmd …`.

## [1.0.1] — 2026-11-10

### Fixed (docs)

- **The README never named `validate` or `book`**, and `view` only appeared by accident
  of the word "previewer". Three working commands were invisible from the front page —
  which is the page a new reader actually reads.

### Added

- **A command index in the README**: thirty commands, one line each, plus the note that
  `<command> --help` lists the flags. A test derives the command list from `main.rs` and
  fails when a command is not named here, so the next one added cannot repeat this.

## [1.0.0] — 2026-11-10

### Added

- **`split-column` in the API** (`POST /edit?action=split-column&col=&by=&into=`).
  The CLI had it and the endpoint did not; the doc said it did, and the doc test that
  compares documented parameters against `serve.rs` caught the lie.

### Changed

- Version 1.0.0. The surface has been stable for twenty versions: the file layout
  (base + journal + view + idx), the error contract, the cost model, and the CLI/API
  parity all hold, and the last twenty rounds have been spent making the parts agree
  with each other rather than adding parts.

## [0.99.0] — 2026-11-10

### Fixed

- **`docs/API.md` was the changelog.** It had been overwritten with the changelog body
  around 0.70 and nothing noticed for twenty-nine versions: anyone who opened the API
  reference to build a client got release notes. Restored from `c6ecf68`, then brought
  current — `POST /index`, `redo`, `split-column`, `regex`, the `/rows` count fields,
  `/sheet`'s `indexed`, and the four path messages.
  - A test now derives the route list from `serve.rs` and fails if a route is missing
    from the doc, so the two cannot drift again silently.
  - The file also has to start with `# HTTP API`, which is the check that would have
    caught the overwrite the same day it happened.

## [0.98.0] — 2026-11-10

### Fixed

- **`/sheet` advertised ten capabilities while the API accepted eleven actions**, and
  the two error hints listed a third, different subset. So a front-end drawing buttons
  from `capabilities` could not offer `redo`, `copy`, `formula`, `split-column` or
  `paste` — implemented, working, invisible. And neither hint could be trusted as a
  description of the endpoint; the "unknown action" message left out `redo` and `copy`
  while the "action is required" one left out `drop`, `rename`, `formula` as well.

  Both now come from one `EDIT_ACTIONS` constant, and `capabilities` lists all eighteen
  things a sheet can do. Tests check both directions: every journal operation has a
  capability, and every advertised capability has an action that answers to it.

## [0.97.0] — 2026-11-10

### Added

- **`<command> --help` now lists that command's own options.** The global help is a task
  view ("what do you want to do → which command"); it cannot describe sixty-one flags
  without becoming a wall. Sixty of them were parsed and never documented — `--get`,
  `--max-cells`, `--qualify`, `--check`, `--regex`, `--sum`, and others that change the
  answer rather than the formatting.

### Fixed

- **`view` had no `--help` at all** — the most-used command answered "unknown option:
  --help". Found by the new test, which parses `main.rs` for the flags each command
  really accepts, runs `<command> --help`, and compares. A flag added without a line
  here now fails the suite instead of staying invisible.

## [0.96.0] — 2026-11-10

### Fixed

- **`?path=` reported one message for four different mistakes.** A typo'd filename,
  a directory, a path outside the root and a missing parameter all said "path missing
  or outside root" — which sends people looking for a sandbox problem that does not
  exist. Each now has its own sentence, and all 17 read endpoints use the same
  resolver, so the improvement is not partial.
  - `path is required` / `no such file inside the served directory` /
    `path is a directory, not a file` / `path is outside the served directory`.

### Verified

- The error contract holds across the whole surface: every 4xx carries `error`, and
  parameter problems also carry `parameter` and `hint`. A test walks the 17 read
  endpoints and checks the shape rather than trusting that each handler remembered.

## [0.95.0] — 2026-11-10

### Added

- **`validate`**: column constraints for dirty data. `--check "qty>=0"`, `--check
  "city:not-empty"`, `--check "date:date"`, `--check "id:unique"` — or any comparison
  the filter syntax understands (`city in CDMX,Puebla`).
  - **Read-only by design**: it applies the journal like every other read, so it checks
    what you would export, and writes nothing. The test asserts the file is
    byte-identical afterwards.
  - Memory stays flat: rows are checked in pages of 2 000 through the pager. Per-rule
    counts are exact even when the detail list is capped at 1 000, and `unique` says so
    when it runs out of room instead of quietly forgetting values.
  - Dates are checked against the calendar, not just the shape: `2026-02-30` is
    refused, `2024-02-29` is accepted, `2026-02-29` is not.
  - Exit 1 when anything is wrong, so `validate && …` works in a pipeline.

## [0.94.0] — 2026-11-10

### Added

- **`search --regex` and `view --find --regex`** (`GET /search?regex=1`): the pattern
  subset conditional formatting already uses — `. * + ? ^ $ [a-z] \d` — now also
  searches. `--find "C1[0-9]"` locates the first city in C10..C19 instead of reporting
  that no cell contains that literal string.
  - One pattern engine, so a rule and a search agree on what a pattern means.
  - Anchors follow the same rules: `^C3$` matches the whole cell.

### Verified

- Every limit in the surface is honest: `/search`, `/filter` and `/query` report
  `truncated`, `chart`, `groupby`, `pivot`, `dedupe` and `lookup` refuse to run past
  their cap with an actionable message, and `sql` prints "(truncated)" beside the row
  count. This round checked them one by one rather than assuming.

## [0.93.0] — 2026-11-10

### Fixed

- **`/rows` silently capped the page size.** `count` is clamped to 2000 — correctly, it
  is what keeps a request from materialising a million rows — but the response said
  nothing about it, so a client asking for a million got 2000 rows and no way to know.
  Every page now carries `count` (rows actually returned), `requested` (what was asked
  for) and `limit` (the ceiling), on both the raw and the edited path.

Verified while probing the boundaries: `count=0` returns an empty page, negative or
non-numeric `start`/`count` are 400 with an actionable hint, and the clamp holds
regardless of what is asked for.
## [0.92.0] — 2026-11-10

### Fixed

- **`POST /edit?action=delete&count=N` deleted N−1 rows.** The handler subtracted one
  from both parameters, because `row` arrives 1-based and is stored 0-based — but
  `count` is not a row number. `count=3` removed 2, and `count=1` removed **nothing**
  while reporting success. Ten rows asked for, nine gone.
  - `count=0` is now a 400 instead of a silent no-op.

- **`view` on an empty screen said "(no rows matched)" for every kind of empty**, so an
  offset past the end looked like a filter that found nothing. It now distinguishes
  "the file has no data rows", "start row 500 is past the end: 209 data rows", and
  "no rows matched; the file has 209 data rows".

The pager itself was already right: paging follows inserts and deletes, and the window
shifts with them. The bug was one arithmetic step earlier.
## [0.91.0] — 2026-11-10

### Verified

- **Derived values read the current row, edits included.** Probed the last unchecked
  member of the family that produced three bugs in a row: a formula reading an edited
  cell, `lag` reading an edited previous row, `running_total` folding edits in, and a
  formula reading a split piece. All four were already right — so this round pinned
  them in a test and wrote the rule down (`docs/CAPABILITIES.md` → "派生值的语义")
  instead of leaving it to be rediscovered.
- No code changed for this; the value is that the next change to the projection path
  has something to break.

## [0.90.0] — 2026-11-10

### Fixed

- **Editing one `split-column` piece wiped the others.** `split full → first,last,addr`
  followed by `set first = Z` exported `Z,,'` — the split re-read the *edited* first
  piece as its source and had nothing left to divide. Pieces are now derived from the
  base text while the row is projected, and an explicit edit wins for its own column,
  so `Z,Perez,St 1` is what comes out.

Same shape as last round's bug: a derived thing was recomputed from a value that
something else had already overwritten. The unit test that "passed" was asserting
inside `ColumnPlan::apply` — where the pieces no longer get written — so it now pins
the mapping the pipeline consults instead.
## [0.89.0] — 2026-11-10

### Fixed

- **Renaming a column silently dropped the edits already made to it.** Edits are keyed
  by column name, so `set qty=99` followed by `rename qty → amount` exported the base
  value: 20 instead of 99, with no warning. The rename now re-keys pending edits,
  substitutions and `where` rules, so a change follows the column the way it follows a
  row.
- **A formula written before the rename stopped compiling.** `formula total = qty*2`
  then `rename qty → amount` made the export fail with "formula reads unknown column
  'qty'". Renames are now applied to formula text before parsing — identifiers only,
  leaving string literals alone, and not matching inside longer names (`qty_total`).

Both were found by the round's question — "do edits survive column changes?" — not by a
report. The row-identity machinery was fine; the column identity was a name string with
no equivalent care.
## [0.88.0] — 2026-11-10

### Added

- **`--quotes` reaches `columns` and `search`**, the two commands from last round's audit
  that decide what data means: `columns` can now reshape a backslash-escaped file
  without mangling it, and `search` can look in the right field.
  Remaining exceptions are `sort`, `concat` and `sql` — `sort` compares keys parsed from
  bytes, `concat` only checks headers, and `sql` goes through DuckDB's own reader.

## [0.87.0] — 2026-11-10

### Fixed (docs)

- **`docs/CAPABILITIES.md` claimed `--quotes` works for "every command that reads
  CSV"**. It does not: `sort`, `columns`, `concat`, `search` and `sql` do not accept it
  and read as RFC 4180. That sentence would have taught a user to pass
  `sort --quotes backslash` and silently get a mis-parsed file — the exact failure this
  feature exists to prevent. The claim now names the eleven commands that do accept it,
  and a test runs each of them to confirm the flag is parsed at all.

### Verified

- The rest of the capability table was checked against the code and the running binary:
  UTF-16 index refusal, single-statement and read-only SQL, every filter operator,
  the pivot/groupby/chart/book limits, and `lookup --max-keys`. Those hold; the audit
  table in the doc records what was checked.

## [0.86.0] — 2026-11-10

### Docs

- **The cost boundary is written down.** "No lag on any size" is a claim about *memory*,
  and the README now separates three kinds of time cost: constant (open, page, view the
  edited sheet), once (building the index), and per-query (filter materialisation, sort,
  column profile — the operations whose job is to read the whole file).
  - Re-measured on 60 MB / 6 M rows: first `/rows` 0.55 s, later pages 0.21 s, a filtered
    second page 18 ms once materialised (0.91 s the first time).
  - The earlier guess — "where= scans every time" — turned out to be wrong: the result
    cache serves the slices, and the scan happens once per distinct query. Writing down
    the measured numbers rather than the remembered ones is the point of the exercise.

## [0.85.0] — 2026-11-10

### Added

- **`GET /sheet` reports `indexed` and `indexBytes`**, and **`POST /index`** builds the
  sparse index as a job. Paging a big file without an index pays for a full scan inside
  the first request — a client that knows it will page can pre-warm and watch the job
  instead of eating the scan under a timeout.
  - The CLI already says when it is building one; this is the same information for
    clients that have no stderr to read.

## [0.84.0] — 2026-11-10

### Fixed

- **The index `Pager` built was never saved.** It scanned the file, used the result once,
  and threw it away — so every page of a big file paid for the scan again, and every new
  process started from scratch. It is saved now: 60 MB file, first page 2.1 s, second
  page 0.3 s.

### Added

- **A notice before building an index** on a file over 50 MB:
  `note: building a row index for big.csv (61.9 MB) — one-time; run `csvx index`
  earlier to avoid the wait`. A silent wait is the worst outcome — the user presses
  Enter, nothing happens, and assumes it hung.

## [0.83.0] — 2026-11-10

### Added

- **`chart` draws in the terminal**: one line per category, bars scaled to the widest
  value, the number beside each bar. `--json` still returns the JSON the API produces
  and `-o` still writes a file, so nothing that worked before changed shape.
  - Numbers are short by design (15, 4.2k, 1.3M): a chart is read at a glance, and a
    full float next to a 40-character bar is noise.
  - A category with no value says `(no data)` instead of drawing a zero-length bar,
    because "no rows" and "zero" are different answers.

## [0.82.0] — 2026-11-10

### Changed

- **`pivot` prints the matrix by default**; `-o` still writes a file. A pivot is
  something you look at, and "write a file, then open the file" was a step nobody
  asked for. The matrix is bounded by `--max-cells`, so printing it is safe.
  - `core::pivot` now separates `pivot_rows` (returns the table) from `pivot_quotes`
    (writes it), so the two cannot drift and the API can return the same table later.

## [0.81.0] — 2026-11-10

### Added

- **`view --rule` / `--rules`**: conditional formatting is visible in the terminal, not
  only as JSON from the API. A terminal gets real colour (named colours and `#rrggbb`
  through ANSI); a pipe gets the cell in brackets, because escape codes in a captured
  file are noise while the marker still says which cell matched.
  - The rules are the same `format::Rule` the API evaluates, so "why is this cell red"
    has one answer.
  - A rule that matches nothing prints nothing extra, rather than a "0 cells" line
    that trains people to ignore the notice.

## [0.80.0] — 2026-11-10

### Added

- **`edit export --where`** (`POST /edit?action=export&where=…`): materialise only the
  rows that match. The predicate sees the finished row — edits and formulas applied —
  so a filter on a computed column behaves the way the spreadsheet does, and an edit
  that makes a row match puts it in the export.
  - Streaming: rows that do not match are skipped as they are read, so a selective
    filter costs nothing extra.

## [0.79.0] — 2026-11-10

### Added

- **`edit split-column`**: Excel's "text to columns". `--col full --by "," --into
  "first,last,addr"` replaces one column with three, in place, and the **last piece
  keeps the remainder** — so `Ana,Perez,Calle 1` becomes `Ana | Perez | Calle 1`
  instead of dropping the commas out of the address. Fewer pieces than names leaves
  the rest empty rather than shifting values into the wrong column.
  - It is a journal operation, so the base file is untouched and `edit undo` takes it
    back like any other edit.
  - The split is computed per row at export time: no extra storage, no extra pass.

## [0.78.0] — 2026-11-10

### Fixed

- **Zip-slip in `extract`.** A zip entry named `../escaped.txt` was joined to the output
  directory and written one level up — verified before the fix, refused after it. Every
  member name is now validated (absolute paths, `..`, backslashes, drive prefixes) and
  **validated before anything is written**, so a malicious archive leaves no partial
  output. Tar already relied on the `tar` crate, which skips unsafe members.
- `extract` reported its errors as `✗ convert:` — a copy-paste from the neighbouring
  command that made a security message look like a transcoding failure.

## [0.77.0] — 2026-11-10

### Added

- **The input file is protected from `-o`.** Writing to the file being read is data
  loss waiting for a typo — `filter big.csv --where … -o big.csv` used to succeed and
  destroy the source. Nine commands now refuse it with exit 1 and an explanation.
- Overwriting *another* existing file is still allowed (rerunning a pipeline is normal),
  but it is announced: `note: overwriting existing out.csv`.

## [0.76.0] — 2026-11-10

### Added

- **`view --find TEXT --nth N`** walks to the second, third, … match without re-running
  the search — the "find next" a spreadsheet has. Asking past the last match explains
  itself: `(only 3 row(s) contain "CDMX" — --nth 9 is past the last one)`.
- A `--find` page is now sized to the context (`--context 3` shows seven rows) instead
  of a full 20-row page, unless `--count` was given explicitly.

## [0.75.0] — 2026-11-10

### Added

- **`view --find TEXT [--context N] [--match-case]`**: one command instead of two. The
  match is located, the page is centred on it, and the row number is reported
  (`row 41 of 100`) — so nobody has to copy a number from `search` into `--start`.
  A miss is a normal answer (`(no row contains "...")`), not a failure.

  The first implementation called `window()` per 2 000-row chunk, which rebuilt the
  index and compiled the formulas fifty times for a million rows: 49 s to scan 1 M
  rows. With one `Pager` for the scan it is 1.4 s — the same lesson as 0.73.0, learned
  again in a new code path, which is why `window()` being a thin wrapper over `Pager`
  was worth doing.

## [0.74.1] — 2026-11-10

### Changed

- **Both caches share one invalidation type** (`Stamp`): the journal is identified by
  a hash of its contents, the base file by size and modification time (it can be 10 GB,
  and it is supposed to be immutable). Two caches asking "has this changed?" now
  cannot answer it differently.

### Verified

- The materialised copy behind `/rows?where=` was *already* invalidating correctly —
  but that was luck, not design: it compared the same tuple the pager cache had been
  comparing wrongly. A test now pins it, and another pins that replacing the base file
  is noticed.

Measured on 200 k rows with one edit: first request 41 ms, every later page 6 ms,
including a page at row 150 000.

## [0.74.0] — 2026-11-10

### Added

- **The server keeps opened sheets across requests** (`Pager` cache, keyed by path,
  journal hash, base file stamp and delimiter). Scrolling no longer re-reads the
  journal and recompiles formulas per page.

### Fixed

- **The API counted rows differently from the CLI.** After the CLI moved to 1-based
  rows, `POST /edit?row=1` still meant "the second data row": one number, two meanings
  depending on which side you asked. The API is 1-based now and `row=0` returns 400
  explaining why.
- **The cache could serve a stale page.** Keying on the journal's `(mtime, len)` missed
  same-tick writes — exactly what the first test did — so the page came from the pre-edit
  sheet. The journal is small and append-only, so its contents are hashed instead:
  a change detector, not a timestamp guess.

## [0.73.0] — 2026-11-10

### Changed

- **`Pager` opens the sheet once.** `window()` re-derived the index, column layout and
  compiled formulas on every call: fine for one HTTP request, wasteful for a pager that
  walks a file. `view` now holds a `Pager` across pages, so a page costs its rows and
  nothing else — 600 k rows, page at row 500 000: 0.9 s including the index build.
  `window()` is now `Pager::open(...).page(...)`, so there is one implementation.
- `view` reports where the page sits: `(page at rows 500001-500010 of 600000)`.

### Fixed

- Two `concat` tests shared a temp directory name when they started in the same
  nanosecond, which is how one of them saw another's missing file.

## [0.72.0] — 2026-11-10

### Added

- **`view --order COL [--desc]`**: paged, sorted view of the edited sheet. Sorting
  cannot be streamed — the smallest key may sit on the last row — so the rows are
  materialised once, filtered, sorted, and the requested page is read back. The
  temp copy holds the matched rows, not the file.
- **Tables line up with CJK text.** Padding used `chars().count()`, which assumes one
  terminal column per character; a Chinese column name shifted every row after it by
  one column per character. Width is now East-Asian-width aware, and `preview` and
  `view` share it.

## [0.71.0] — 2026-11-10

### Added

- **`view <file> [--where EXPR] [--start N] [--count M] [--json]`**: a paged look at
  the *edited* sheet in the terminal. It reads in windows instead of loading the file,
  applies the journal, and reuses the same compiled predicate the exporter uses — a
  second implementation would eventually disagree about what `city=CDMX` means.
  Filtering streams: each window is tested and printing stops as soon as the
  requested count is reached, so a selective filter over 10 GB stays cheap.

## [0.70.0] — 2026-11-10

### Added

- **Window reads answer `lag()`** by pre-reading the rows before the window: `k` rows
  where `k` is the largest look-back, not `S` rows where `S` is the window start. A
  paged view now shows the same previous-row values an export does instead of
  `#VALUE!`; the two paths disagreed, which is a bug waiting for someone to trust the
  wrong one.
- `running_total` still reports `#VALUE!` in windows: it needs every row before the
  start, and a full pass per page is the wrong trade. The alternative — totalling only
  the window's first row onward — would look plausible and be wrong.

## [0.69.0] — 2026-11-10

### Changed

- **Export and window reads share one row assembler** (`RowPipeline`). They had a
  copy each, and adding `lag` in 0.67.0 meant writing it twice — which is how the
  window copy ended up reporting `#VALUE!` for history it could have had. The
  pipeline owns the row state, so history advances exactly once per row during
  export and is absent in windows.

## [0.68.0] — 2026-11-10

### Changed

- **`workbook.rs` (2500 lines) split into four modules**: `mod.rs` (Op, Sheet,
  column layout), `rows.rs` (row identity, the logical→physical map, cell edits,
  formula compilation), `journal.rs` (the journal and its text format), and
  `export.rs` (export and windowed reads). The boundaries follow change rate rather
  than line count: the journal format is nearly frozen, export touches every row.

## [0.67.0] — 2026-11-10

### Added

- **`lag(col[, k])` and `running_total(col)`**: formulas that see earlier rows.
  A ring buffer per referenced column, sized by the largest lag, plus one accumulator
  per running total: 3 M rows / 30 MB exported in 17 MB peak RSS. The first row says
  `#VALUE!` rather than 0 — a zero delta on row one looks like data.

## [0.66.0] — 2026-11-10

### Added

- **A1 references in formulas**: `B2 * C2` means what a filled-down spreadsheet
  formula means — column B and C of *this* row, relative to the first data row.
  A reference needing another row (`B3`) reports `#VALUE!` instead of reading the
  current row. `[A1]` still means "the column literally named A1".

## [0.65.0] — 2026-11-10

### Added

- **`edit replace --regex`**, reusing the pattern subset conditional formatting
  already had, so "what can a pattern contain" has one answer. A bad pattern is
  refused before it becomes a journal step that does nothing.

### Fixed

- **`sort` is now stable across chunks.** Chunk sorting was stable, but the k-way
  merge broke ties by heap order, so equal keys could come out in an arbitrary
  sequence once a file was split — an almost-stable sort, which is worse than an
  unstable one because you only notice when it reorders something.

## [0.64.0] — 2026-11-10

### Added

- **Redo** (`edit redo`, `POST /edit?action=redo`): undone operations wait in
  `<file>.journal.redo`, so a reader never has to know someone pressed undo. A new
  edit discards that branch, the way a spreadsheet does.
- **Rows are 1-based for the user** (`--row 1` is the first data row), matching A1
  references and every spreadsheet; `--row 0` is refused with an explanation. The
  journal still stores 0-based physical indices, so existing journals stay valid.

## [0.63.0] — 2026-11-10

### Fixed

- **`preview` and `analyze` loaded the whole file.** The quote-style detector I added
  in 0.57.0 used `read_to_end` and then kept 64 KB — so a 322 MB file cost 332 MB of
  memory before showing two rows. One `take(64 * 1024)` fixes it: 8.3 MB for preview,
  17.1 MB for analyze, independent of file size.
  This is the round where "memory does not grow with the file" was actually false for
  two of the most common commands, and the docs had been claiming otherwise.

### Added

- **`scripts/bench.sh`**: re-measures the README's numbers on any file (or a generated
  fixture) and prints a markdown table. Time grows with bytes; memory must not.
- A regression test compares a 6 MB and a 60 MB preview rather than asserting a
  wall-clock figure, because a full read scales with size and noise does not.

## [0.62.0] — 2026-11-10

### Added

- **Every read endpoint's error shape is verified**: a test walks 17 endpoints with
  missing or malformed parameters and requires `error` plus `parameter`/`hint` — no
  prose, no silent 200.
- **Numeric parameters are validated instead of ignored**: `/rows?start=abc` and
  `/book?max=nope` now return 400 naming the parameter. Silently falling back to the
  default means a caller pages with the wrong window and blames the data.
- `scripts/smoke.sh` probes the API for the same thing, so the check runs against a
  live server too.

## [0.61.0] — 2026-11-10

### Added

- **`scripts/smoke.sh`**: 26 steps covering one user's session — demo → view →
  filter → sort → groupby → pivot → chart → format → layout → edit → export →
  lookup → split/verify/merge/partition → server. Plus combination paths: GBK with a
  semicolon delimiter, a multi-line quoted field round-tripped byte for byte, and the
  edited-file warning.

### Fixed

- **Six commands failed without printing anything.** `pivot`, `groupby`, `chart`,
  `convert`, `lookup` and `columns` return their error instead of sending it through
  the progress channel, so the reporter stayed silent and the match arm discarded it:
  a misspelled column produced exit code 1 and a blank screen. All now print
  `✗ <command>: <reason>` with a hint, and two tests keep it that way —
  `every_failure_prints_a_reason` and `usage_errors_are_never_silent` (which walks
  every sub-command with no arguments).

### Changed

- `--help` opens with **"what do you want to do?"** — a task index mapping goals to
  commands, because 22 sub-commands listed by name is a reference, not an
  introduction. A test checks the index only names commands that exist.

## [0.60.0] — 2026-11-10

### Added

- **`--quotes` reaches every command that reads CSV**: `chart`, `dedupe`, `stats`,
  `analyze` joined `preview`, `filter`, `groupby`, `pivot`, `lookup` and `edit export`.
  Each module gained a `*_quotes` entry point and kept its previous function as the
  RFC-default wrapper, so no existing caller changed.
- The record reader is shared, so the fix applies to record boundaries as well as
  fields: a backslash file with `\"` no longer merges the next line into the previous
  record.

`docs/CAPABILITIES.md` loses its "quote style only covers two commands" line — the
first gap on that list that has actually been closed rather than reworded.

## [0.59.0] — 2026-11-10

### Added

- **Quote style reaches the record boundary, not just the field split**
  (`RecordReader::new_with`). This was the real bug behind the `export --quotes`
  attempt: in a backslash-escaped file a `\"` does not close a quoted section, so an
  RFC reader runs past the record and swallows the next line into it. Field-level
  parsing cannot fix a record that was never separated correctly.
- `--quotes` now applies to `preview`, `filter`, `groupby`, `pivot`, `lookup` and
  `edit export` — the commands that either decide matches or write new files.
- Tests pin the record count under both styles (3 vs 2 for the same bytes), because
  "the row count is wrong" is a much quieter failure than "the field looks odd".

## [0.58.0] — 2026-11-10

### Added

- **`--quotes` reaches `filter`**, not just `preview`. Reading a backslash-escaped
  file as RFC changes what a filter *matches*, not merely how output looks: the note
  field ends at the escaped quote, so `note~c` misses a row that contains `c`. A test
  pins both outcomes.
- `filter::extract_style` is the style-aware entry point; `extract` keeps the RFC
  default so every existing caller is unchanged.

## [0.57.0] — 2026-11-10

### Added

- **Quote-style detection and override** (`crates/core/src/csv_reader.rs`):
  RFC 4180 (default), `backslash` (`\"` / `\\` escapes), `none` (no quoting).
  - A file that escapes with backslashes is detected from its first 64 KB and the
    CLI says so: *"this file looks like it escapes quotes with a backslash — if
    fields look wrong, re-run with `--quotes backslash`"*.
  - `preview --quotes backslash|rfc4180|none` reads it correctly.
  - Why detection matters at all: reading a backslash file as RFC does not fail. It
    merges or splits fields silently — `Ana | has a \quote\ inside` instead of
    `has a "quote" inside`, and `["a", "b\\c,d"]` instead of three fields. A test
    pins that exact wrong output so the failure mode stays documented.

## [0.56.0] — 2026-11-10

### Added

- **`analyze --sample N`**: profile the first N rows instead of the whole file.
  Measured on 1 M rows: 1.7 s → 0.35 s.
  - It is a **prefix** sample, not a random one — a random sample needs the whole
    file (or an index) to pick from, which defeats the purpose. So the command says
    so on screen: "sampled the first N rows (a prefix, not a random sample) — counts
    are estimates". A user who reads the sampled row count as the file's row count
    will draw wrong conclusions, and the tool must not help them.

## [0.55.0] — 2026-11-10

### Added

- **`filter --order`**: filter and sort in one command —
  `filter big.csv --where "amount>100" --order "city,amount:desc" -o out.csv`.
  Filtering stays streaming; the matched rows are staged to a temp file and sorted
  into place, so only matches touch disk twice.
- Multi-key sorting (`--by city,amount:desc`) was already implemented in `sort` but
  undocumented; the help and README now say so, and a test covers both paths.

## [0.54.0] — 2026-11-10

### Docs

- **`docs/API.md`**: the HTTP surface written for someone building a front-end —
  conventions first (error shape, window shape, job shape), then every endpoint with
  its parameters, then copy-pasteable examples for the ten things people actually do.
  Answering "what does /style expect?" used to mean reading `serve.rs`.
- Two tests keep it honest: every endpoint in the doc must exist in the router, and
  every parameter named in the doc must be read by the server somewhere. Documentation
  drift was the first bug this project ever had and it will not be the last.

## [0.53.0] — 2026-11-10

### Added

- **`row()` in formulas**: the 1-based row number, so per-row values no longer need
  per-row storage. `order & "-" & row()` builds a sequence column;
  `if(row() % 2 = 1, "odd", "even")` alternates; both cost one evaluation per row.
  - Evaluation now carries a small context (cells, references, row number) instead
    of two loose parameters, which is what made adding row awareness mechanical
    rather than a rewrite.
- **`POST /edit?action=copy&from=A1:B2&to=B3`**: the block-copy CLI capability, now
  reachable from the API, sharing the same range parser
  (`workbook::parse_a1_range`) so the two cannot disagree about what `A1:B2` means.

## [0.52.0] — 2026-11-10

### Added

- **Block copy/paste with A1 ranges**: `edit copy --from "A2:C5" --to "B10"`.
  - The pasted block stores **values**, not a reference to the source: storing a
    reference would let a later edit of the source silently rewrite what had already
    been pasted.
  - Ranges are parsed the way people write them (`B3`, `A2:C5`, `AA10`), with the
    bounds checked against the sheet rather than producing an empty paste.
  - Bounded at 100 000 cells — beyond that the right tool is `edit set --where`
    (one rule) or an export, not a hundred-thousand-value journal entry.
  - Paste is anchored to **row identities**, so inserting rows above moves the
    pasted block along with the rows it landed on.

## [0.51.0] — 2026-11-10

### Added

- **Insert a column where it belongs** (`edit insert-column --after city --name note`):
  a constant (`--value`) or a computed one (`--formula`). Appending to the far right
  was the only option before, which is not what "add a note column next to city"
  means.
  - `ColumnPlan` now describes every derived column once per export/window — formulas
    parsed once, literals cloned once — instead of a `formula_index` that could only
    express formulas.
  - Available through `POST /edit?action=insert-column`, with `--value` and
    `--formula` mutually exclusive rather than silently preferring one.

## [0.50.0] — 2026-11-10

### Added

- **Bulk edit by filter**: `edit set --where "city=CDMX" --col amount --value 0`
  changes every matching row, and the API accepts `action=set&where=…`.
  - Recorded as **one rule**, not one operation per row: a million-row bulk edit is
    a 28-byte journal entry. The rule is evaluated while the sheet is read, so it
    sees values created by earlier edits and formulas can read its result.
  - `--row` and `--where` together are a usage error rather than a guess.

## [0.49.0] — 2026-11-10

### Added

- **Frozen columns are a real behaviour now, not just metadata.** Freezing is a
  *prefix* — pinning the first two columns means something, pinning only the third
  means the first two scroll away — so `Layout::frozen_prefix` reports how many
  leading columns are pinned, and `/rows` plus `/sheet` return `frozenColumns`.
  A client splits its table into a pinned pane and a scrolling pane; the row data
  is not duplicated because the values are already there.
- `layout --freeze-first N` sets the prefix in one command instead of one `--set`
  per column.

## [0.48.0] — 2026-11-10

### Added

- **Cross-sheet references in formulas** (`crates/core/src/formula/sheetref.rs`):
  `ref("customers.csv", "cust_no", "name")` looks up this row's `cust_no` in another
  sheet and returns its `name` — a VLOOKUP inside a formula column.
  - The referenced sheet is loaded once into a map; the sheet being computed stays
    streamed, so the asymmetry that makes `lookup` cheap applies here too.
  - Sheet names resolve **relative to the base file's directory**, so moving a
    directory does not break a formula.
  - A missing referenced sheet fails **at export/open time**, not as a million
    `#N/A` values discovered later; a missing key inside the sheet is `#N/A`, like
    VLOOKUP.
  - `Refs` is built from the formula's own literals (`Formula::sheet_references`),
    which is why dynamic sheet names are refused rather than silently returning
    nothing.

## [0.47.0] — 2026-11-10

### Added

- **Workbook view** (`crates/core/src/book.rs`): a directory is a workbook —
  `book <dir>` / `GET /book` lists every CSV with size, header width, row count
  (when an index exists), pending edits, and whether it carries layout metadata.
  Row counts come from the sparse index rather than a scan, so listing a folder
  never turns into reading the folder.
- **`docs/CAPABILITIES.md`**: one page covering what every capability does, which
  CLI command and endpoint serve it, and where each one's ceiling is (max groups,
  max cells, max keys, max points). Scattered README sections were the reason
  "can this do X?" required reading four files.

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
  `csvx <command>` in a code block must be a real sub-command, every
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

- **`csvx demo`**: writes a realistic sample file (20,120 rows, 918 KB)
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

- **`csvx serve`**: a dependency-free local HTTP API for a separated
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
  byte offset of every 1000th record, plus `csvx index` to build it.
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

- **CLI** (`crates/cli`, binary `csvx`): `split`, `merge`, `preview`,
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

## [0.1.0] — 2025-11-10

Initial release: split + compress pipeline, preview, column stats, decompress,
bilingual GUI.
