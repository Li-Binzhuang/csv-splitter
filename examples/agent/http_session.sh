#!/usr/bin/env bash
# Create one Agent session, call a read tool, print the audit/actions, and
# export replay/curl payloads. Assumes `csvx agent serve` is already running.
set -euo pipefail

api=${CSVX_API:-http://127.0.0.1:8788}
root=${CSVX_ROOT:-$PWD}
file=${CSVX_FILE:-t.csv}
agent=${CSVX_AGENT:-example}

payload=$(curl -sS -X POST "$api/agent/v1/sessions" \
  -H 'Content-Type: application/json' \
  -d "{\"agent\":\"$agent\",\"root\":\"$root\"}")

token=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])' <<<"$payload")
session=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["sessionId"])' <<<"$payload")

echo "session=$session"
echo "file=$file"
echo "--- action"
curl -sS -X POST "$api/agent/v1/sessions/$session/actions" \
  -H "Authorization: Bearer $token" \
  -H 'Content-Type: application/json' \
  -d "{\"tool\":\"csvx_schema\",\"arguments\":{\"path\":\"$file\"}}"
echo
echo "--- actions.md"
curl -sS -H "Authorization: Bearer $token" \
  "$api/agent/v1/sessions/$session/actions.md"
echo
echo "--- replay JSONL (save with -o)"
curl -sS -H "Authorization: Bearer $token" \
  "$api/agent/v1/sessions/$session/replay"
echo
echo "--- close"
curl -sS -X POST "$api/agent/v1/sessions/$session/close" \
  -H "Authorization: Bearer $token" \
  -H 'Content-Type: application/json' -d '{}'
echo
