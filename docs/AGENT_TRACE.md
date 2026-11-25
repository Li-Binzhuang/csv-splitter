# Agent trace / replay

`csvx agent run` can record every model response and tool call, and replay the
tool sequence without a model.

## Record

```bash
csvx agent run ... --trace /tmp/run.jsonl
```

Trace is JSONL:

```json
{"event":"run","endpoint":"...","model":"...","root":"...","prompt":"...","skill":"...","maxSteps":5}
{"event":"model","step":1,"content":"{\"tool\":\"csvx_schema\",...}"}
{"event":"tool","step":1,"tool":"csvx_schema","arguments":{"path":"t.csv"},"observation":"...","elapsedMs":12}
{"event":"model","step":2,"content":"{\"tool\":\"csvx_report\",...}"}
{"event":"tool","step":2,"tool":"csvx_report","arguments":{...},"observation":"...","elapsedMs":600}
{"event":"final","step":3,"answer":"..."}
```

- No API key is written.
- `observation` is truncated to 20,000 chars.
- The trace can contain source data and row values; treat it as data, not a
  shareable artifact.

## Replay

```bash
csvx agent replay --root /path --replay /tmp/run.jsonl
```

It runs only the `tool` events, in order, using the same `mcp_run_tool` executor.
It does not call a model and does not require a key. `model` and `final` events
are ignored for execution; the final answer stays in the trace for inspection.

## HTTP Agent API

The HTTP side has the same idea: `GET /agent/v1/sessions/{id}/audit` (alias
`/trace`) returns the raw JSONL for **that session only** (the shared file is
filtered by session id), with `tool`, `arguments`, `status`, `elapsedMs` and an
output preview. It is written on `running`, `approved`, and again on
`ok/error`, so a failed action is still visible. Tokens are never written.

Replay HTTP audit lines too:

```bash
curl -sS -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:8788/agent/v1/sessions/$SESSION/replay \
  > session.jsonl

csvx agent replay --root /path/to/data --replay session.jsonl
```

`/replay` already removes `running`/`approved` lines; `/audit` keeps the full
history. `replay` executes only final `ok`/`error` lines either way.

For a human-readable command sequence, `GET /sessions/{id}/curl` returns a
shell script:

```bash
export CSVX_TOKEN=...
export CSVX_SESSION=...
export CSVX_API=http://127.0.0.1:8788
curl -sS -H "Authorization: Bearer $CSVX_TOKEN" \
  "$CSVX_API/agent/v1/sessions/$CSVX_SESSION/curl" | bash
```

The script contains tool arguments and uses `$CSVX_TOKEN`/`$CSVX_SESSION` placeholders.
Destructive calls carry `"approvalId":"__APPROVAL_ID__"` plus a ready-to-run
`/approvals` command; the comment says whether the recorded audit had an
approval. Replace the placeholder with the returned id before running. `GET /actions` gives the same final actions as JSON; `/actions.md` and
`/actions.html` render them as a table (step, approval state, status) for a run
log or issue comment; the Markdown response is marked as an attachment.

## Why

- You can diff the model's chosen tools and arguments against a hand-written
  workflow.
- You can re-run the exact tool sequence after changing code.
- Replay is not a substitute for the approval policy: destructive tools still
  run through their normal CLI/journal path, so replay should be used on a copy
  of the data when edits are involved.

## Verified

A run against `applist_svd_v2_1795572689385_result.csv` recorded 2 tool calls
(`csvx_schema`, `csvx_report`) and returned the eight date counts. Replay
executed both tool calls without a model and printed the same report.

An automated test covers replay with a synthetic trace:
`agent_replay_runs_recorded_tool_calls_without_a_model`.
