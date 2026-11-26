#!/usr/bin/env bash
# Smoke diagnostic bundle: contents, counters and include-logs boundary.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${CSVX_BIN:-target/debug/csvx}"
[[ -x "$BIN" ]] || BIN=target/release/csvx
[[ -x "$BIN" ]] || { echo "ops_bundle=fail no binary" >&2; exit 2; }
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PREFIX="$WORK/prefix"; ROOT="$WORK/root"
mkdir -p "$PREFIX/bin" "$PREFIX/share/csvx/docs" "$ROOT/.csvx"
cp "$BIN" "$PREFIX/bin/csvx"
cp docs/SECURITY.md docs/MIGRATION.md "$PREFIX/share/csvx/docs/"
VERSION="$("$BIN" --version | awk '{print $2}')"
PLATFORM="$(uname -s)-$(uname -m)"
python3 - "$PREFIX/install-manifest.json" "$VERSION" "$PLATFORM" <<'PY'
import datetime
import json
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
path.write_text(json.dumps({
    "version": sys.argv[2],
    "sha256": "0" * 64,
    "platform": sys.argv[3],
    "installedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "binary": str(path.parent / "bin/csvx"),
}, indent=2) + "\n")
PY
cat > "$ROOT/.csvx/mcp-audit.jsonl" <<'JSONL'
{"ts":1,"tool":"csvx_schema","ok":true,"code":"ok","ms":1,"args":{"path":{"type":"string","chars":9}}}
{"ts":2,"tool":"csvx_view","ok":false,"code":"cli_error","ms":2,"args":{}}
JSONL
cat > "$ROOT/.csvx/pipeline-history.jsonl" <<'JSONL'
{"run":"a","ok":true,"steps":[]}
JSONL
BUNDLE="$WORK/bundle.tar.gz"
bash scripts/ops_bundle.sh --prefix "$PREFIX" --root "$ROOT" --out "$BUNDLE" >/dev/null
test -s "$BUNDLE" && test -s "$BUNDLE.sha256"
tar -tzf "$BUNDLE" | grep -q './BUNDLE.txt'
tar -tzf "$BUNDLE" | grep -q './ops-status.json'
tar -tzf "$BUNDLE" | grep -q './selfcheck.json'
if tar -tzf "$BUNDLE" | grep -q './state/'; then
  echo "ops_bundle=fail logs included by default" >&2
  exit 1
fi
mkdir -p "$WORK/out" && tar -xzf "$BUNDLE" -C "$WORK/out"
python3 - "$WORK/out/ops-status.json" "$WORK/out/selfcheck.json" <<'PY'
import json
import sys
ops = json.load(open(sys.argv[1]))
check = json.load(open(sys.argv[2]))
assert ops["installOk"] is True, ops
assert ops["mcpAudit"]["entries"] == 2, ops
assert ops["pipeline"]["historyEntries"] == 1, ops
assert check["ok"] is True, check
PY
if grep -R 'person@example\|secret-row' "$WORK/out" >/dev/null 2>&1; then
  echo "ops_bundle=fail raw value leaked" >&2
  exit 1
fi
WITH_LOGS="$WORK/with-logs.tar.gz"
bash scripts/ops_bundle.sh --prefix "$PREFIX" --root "$ROOT" --out "$WITH_LOGS" --include-logs >/dev/null
tar -tzf "$WITH_LOGS" | grep -q './state/mcp-audit.jsonl'
echo "ops_bundle=ok include_logs=explicit"
