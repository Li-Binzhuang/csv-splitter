# csvx Agent quickstart

Three ways to attach an agent; all use the same 23 MCP tools and the same
`root` boundary.

## 1. MCP (stdio)

```json
{
  "mcpServers": {
    "csvx": {
      "command": "/path/to/csvx",
      "args": ["mcp", "--root", "/path/to/data"]
    }
  }
}
```

`initialize` returns behavioral instructions; `resources/read csvx://agent`
returns the manifest with tools, workflow, safety and HTTP endpoint links.

## 2. HTTP Agent API

```bash
csvx agent serve --root /path/to/data --addr 127.0.0.1:8788
curl -sS -X POST http://127.0.0.1:8788/agent/v1/sessions \
  -H 'Content-Type: application/json' -d '{"agent":"demo"}'
```

Then use the returned one-time token:

```bash
curl -sS -X POST http://127.0.0.1:8788/agent/v1/sessions/$SESSION/actions \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"tool":"csvx_schema","arguments":{"path":"t.csv"}}'
```

Explore:

```text
GET /sessions/{id}/audit        full JSONL (tool, arguments, status, duration)
GET /sessions/{id}/actions      final actions + approval state
GET /sessions/{id}/replay       clean actions for `csvx agent replay`
GET /sessions/{id}/curl         copyable curl script with approval hints
GET /sessions/{id}/runbook.md   one-page runbook
GET /sessions                multi-session counts (token required, no tokens returned)
```

Destructive tools need `POST /approvals` first; without it the action returns 428.
A denied approval is 403, and an expired approval is 428 `approval expired`.

## 3. Built-in runner (OpenAI-compatible)

```bash
export CSVX_AGENT_API_KEY=...
csvx agent run --root /path/to/data \
  --endpoint https://.../chat/completions \
  --model DeepSeek-V4.1-Flash-1017-Ark-Economy \
  --skill skills/csvx-data/SKILL.md \
  --prompt 'Use csvx_schema on t.csv, then reply with the row count.'
```

Record and replay:

```bash
csvx agent run ... --trace run.jsonl
csvx agent replay --root /path/to/data --replay run.jsonl
```

## Skill

```bash
bash scripts/install_skill.sh
```

Copies `skills/csvx-data/SKILL.md` to `$DSH_HOME/skills/csvx-data/SKILL.md`.

## Excel

```bash
csvx convert data.csv --format xlsx -o data.xlsx \
  --highlight "amount>1000:red"
csvx report data.csv --spec-text 'datecounts,base_time:status' --format xlsx \
  -o dates.xlsx
```

## Safety

- single user, local only; no GUI;
- paths stay inside root;
- keys are read from the environment and never written to traces/audit;
- approvals are explicit and single-use;
- traces/audit contain data, treat them as sensitive.
