# Agent API examples

No model key is stored here. Start the service first:

```bash
csvx agent serve --root /path/to/data --addr 127.0.0.1:8788
```

Then create a session:

```bash
curl -sS -X POST http://127.0.0.1:8788/agent/v1/sessions \
  -H 'Content-Type: application/json' \
  -d '{"agent":"example"}'
```

The response contains a one-time `token` and `sessionId`. Call a tool:

```bash
curl -sS -X POST http://127.0.0.1:8788/agent/v1/sessions/$SESSION/actions \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"tool":"csvx_schema","arguments":{"path":"t.csv"}}'
```

A destructive tool returns 428 until an approval is issued:

```bash
curl -sS -X POST http://127.0.0.1:8788/agent/v1/sessions/$SESSION/approvals \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"tool":"csvx_edit_apply","decision":"approve"}'
```

Then pass `approvalId` in the action body. Audit is at
`<root>/.csvx-agent/audit.jsonl`.
