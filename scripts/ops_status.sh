#!/usr/bin/env bash
# Local operations snapshot: install self-check + pipeline/audit counters, no network.
set -euo pipefail
cd "$(dirname "$0")/.."
PREFIX="${CSVX_INSTALL_PREFIX:-}"
ROOT="${CSVX_OPS_ROOT:-.}"
AUDIT_DIR="${CSVX_AUDIT_DIR:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --root) ROOT="${2:?--root needs a directory}"; shift 2 ;;
    --audit-dir) AUDIT_DIR="${2:?--audit-dir needs a directory}"; shift 2 ;;
    -h|--help) echo "usage: bash scripts/ops_status.sh [--prefix DIR] [--root DIR]" >&2; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ -d "$ROOT" ]] || { echo "error: root not found: $ROOT" >&2; exit 2; }
ROOT="$(cd "$ROOT" && pwd)"
AUDIT="$ROOT/.csvx/mcp-audit.jsonl"
HISTORY="$ROOT/.csvx/pipeline-history.jsonl"
INSTALL_JSON="null"
INSTALL_OK="true"
if [[ -n "$PREFIX" ]]; then
  ARGS=(--prefix "$PREFIX")
  [[ -n "$AUDIT_DIR" ]] && ARGS+=(--audit-dir "$AUDIT_DIR")
  set +e
  INSTALL_JSON="$(bash scripts/install_selfcheck.sh "${ARGS[@]}" 2>/dev/null)"
  code=$?
  set -e
  [[ "$code" -eq 0 ]] || INSTALL_OK="false"
fi
python3 - "$ROOT" "$AUDIT" "$HISTORY" "$INSTALL_JSON" "$INSTALL_OK" <<'PY'
import json
import os
import sys
root, audit, history, install_json, install_ok = sys.argv[1:]
def lines(path):
    try:
        return open(path, encoding="utf-8", errors="replace").read().splitlines()
    except FileNotFoundError:
        return []
audit_lines = [line for line in lines(audit) if line.strip()]
history_lines = [line for line in lines(history) if line.strip()]
payload = {
    "root": root,
    "install": json.loads(install_json) if install_json != "null" else None,
    "installOk": install_ok == "true",
    "pipeline": {
        "historyEntries": len(history_lines),
        "failedRuns": sum(1 for line in history_lines if line.startswith('{"run"') and '"ok":false' in line),
    },
    "mcpAudit": {
        "entries": len(audit_lines),
        "failedCalls": sum(1 for line in audit_lines if '"ok":false' in line),
        "bytes": os.path.getsize(audit) if os.path.exists(audit) else 0,
        "rotatedBytes": os.path.getsize(audit + ".1") if os.path.exists(audit + ".1") else 0,
    },
    "limits": ["local only", "no network", "not an SLA"],
}
print(json.dumps(payload, indent=2, ensure_ascii=False))
raise SystemExit(0 if install_ok == "true" else 1)
PY
