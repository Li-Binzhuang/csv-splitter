#!/usr/bin/env bash
# Smoke ops_status: install snapshot + local counters, plus bad-prefix path.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${CSVX_BIN:-target/debug/csvx}"
[[ -x "$BIN" ]] || BIN=target/release/csvx
[[ -x "$BIN" ]] || { echo "ops_status=fail no binary" >&2; exit 2; }
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
{"ts":1,"tool":"csvx_schema","ok":true,"code":"ok","ms":1}
{"ts":2,"tool":"csvx_view","ok":false,"code":"cli_error","ms":2}
{"ts":3,"tool":"csvx_filter","ok":true,"code":"ok","ms":3}
JSONL
cat > "$ROOT/.csvx/pipeline-history.jsonl" <<'JSONL'
{"run":"a","ok":true,"steps":[]}
{"run":"b","ok":false,"errorCode":"pipeline_error","steps":[]}
JSONL
OUT="$(bash scripts/ops_status.sh --prefix "$PREFIX" --root "$ROOT")"
printf '%s\n' "$OUT" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["installOk"] is True, d; assert d["install"]["ok"] is True, d; assert d["pipeline"]["historyEntries"]==2, d; assert d["pipeline"]["failedRuns"]==1, d; assert d["mcpAudit"]["entries"]==3, d; assert d["mcpAudit"]["failedCalls"]==1, d; print("ops_status=ok")'
BAD="$WORK/bad-prefix"; cp -R "$PREFIX" "$BAD"
python3 - "$BAD/install-manifest.json" <<'PY'
import json
import sys
path = sys.argv[1]
data = json.load(open(path))
data["version"] = "0.0.0"
open(path, "w").write(json.dumps(data) + "\n")
PY
set +e
bash scripts/ops_status.sh --prefix "$BAD" --root "$ROOT" > "$WORK/bad.json" 2>/dev/null
code=$?
set -e
[[ "$code" -eq 1 ]] || { echo "ops_status=fail bad prefix code=$code" >&2; exit 1; }
python3 - "$WORK/bad.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["installOk"] is False, data
assert data["pipeline"]["historyEntries"] == 2, data
print("ops_status=bad_prefix_rejected")
PY
echo "ops_status=ok"
