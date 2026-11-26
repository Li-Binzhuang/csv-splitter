#!/usr/bin/env bash
# Smoke the synthetic design-partner kit and its refusal path.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${CSVX_BIN:-target/debug/csvx}"
[[ -x "$BIN" ]] || BIN=target/release/csvx
[[ -x "$BIN" ]] || { echo "pilot_kit_smoke=fail no binary" >&2; exit 2; }
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
OUT="$WORK/kit"
CSVX_PILOT_BINARY="$BIN" CSVX_PILOT_OUT="$OUT" bash scripts/pilot_kit.sh >/dev/null
for file in demo/orders.csv pilot-report.md kickoff.json kickoff.md; do
  test -s "$OUT/$file" || { echo "pilot_kit_smoke=fail missing $file" >&2; exit 1; }
done
python3 - "$OUT/kickoff.json" "$OUT/pilot-report.md" <<'PY'
import json
import sys
kickoff = json.load(open(sys.argv[1]))
for key in ("binaryVersion", "sample", "report", "checklist", "limits"):
    assert key in kickoff, kickoff
assert any(item["status"] == "ready" for item in kickoff["checklist"])
report = open(sys.argv[2]).read()
assert "filter" in report and "quality" in report, report[:500]
print("pilot_kit_smoke=json_ok")
PY
if CSVX_PILOT_BINARY="$BIN" CSVX_PILOT_OUT="$OUT" bash scripts/pilot_kit.sh >/dev/null 2>&1; then
  echo "pilot_kit_smoke=fail existing output accepted" >&2
  exit 1
fi
echo "pilot_kit_smoke=ok"
