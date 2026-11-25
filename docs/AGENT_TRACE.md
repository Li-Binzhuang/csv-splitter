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
