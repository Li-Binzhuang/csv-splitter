---
name: csvx-data-workbench
description: "Use when working with CSV or Excel data through csvx: inspect a table, preview and apply edits, run reports/gates, convert xlsx, or call the local Agent API. Covers the MCP tools, csvx agent, approvals and audit."
---

# csvx — local CSV / Excel backend

csvx is a local, single-user backend. It is not a GUI. Prefer reading before
writing, and keep every path inside the configured root.

## Read path

1. `csvx_schema` — columns, types, row count.
2. `csvx_view` — paging, optional `where`.
3. `csvx_quality` / `csvx_gate` for keys, duplicates and thresholds.
4. `csvx_report` for a deterministic Markdown/HTML report from a CSV spec.

## Write path

1. `csvx_edit_preview` with a transaction string:
   `set:1:qty:9;delete:2:1;insert:4:a,b`.
2. Show the affected rows/summary to the user.
3. Only then `csvx_edit_apply` with the same ops.
4. `csvx_edit_undo` / `csvx_edit_redo` use the journal; the source file is not
   rewritten.

## Excel

- Export: `csvx convert data.csv --format xlsx -o data.xlsx`.
- Directory → workbook: `csvx convert parts/ --format xlsx -o book.xlsx`.
- Read: `csvx convert input.xlsx --format csv --sheet Data -o data.csv`.
- Strict ISO `YYYY-MM-DD` values are exported as Excel date cells and read back
  as `YYYY-MM-DD`; other text stays text.
- `--highlight "amount>1000:red"` writes Excel cellIs conditional formatting.

## Agent API

- MCP: `csvx mcp --root DIR`; `initialize` returns instructions; resource
  `csvx://agent` carries the manifest.
- HTTP: `csvx agent serve --root DIR`; create a session, then call
  `/agent/v1/sessions/{id}/actions`. Destructive tools need an approval first
  (HTTP 428).
- Runner: `csvx agent run --root DIR --endpoint ... --model ... --prompt ...`;
  set `CSVX_AGENT_API_KEY` in the environment. Destructive tools need
  `--allow-apply`.

## Safety

- No GUI, no multi-user auth, no telemetry.
- Do not pass model keys to tools or write them to files.
- Do not invent rows, counts or file paths; use tool output.
