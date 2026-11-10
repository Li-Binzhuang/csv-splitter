# Changelog

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
  `note: building a row index for big.csv (61.9 MB) — one-time; run `csv-splitter index`
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

## [0.1.0] — 2025-11-10

Initial release: split + compress pipeline, preview, column stats, decompress,
bilingual GUI.
