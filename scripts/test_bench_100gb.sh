#!/usr/bin/env bash
# Smoke the benchmark harness without generating a large file.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${CSVX_BIN:-target/debug/csvx}"
[[ -x "$BIN" ]] || BIN=target/release/csvx
[[ -x "$BIN" ]] || { echo "bench_smoke=fail no binary" >&2; exit 2; }
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
OUT="$WORK/orders.csv"
CSVX_BIN="$BIN" python3 scripts/bench_100gb.py --rows 1000 --out "$OUT" --clean >"$WORK/run.log" 2>&1
test -s "$OUT.bench.json" || { echo "bench_smoke=fail json" >&2; exit 1; }
test -s "$OUT.bench.md" || { echo "bench_smoke=fail markdown" >&2; exit 1; }
test -s "$OUT.summary.json" || { echo "bench_smoke=fail summary" >&2; exit 1; }
python3 - "$OUT.bench.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["generated"]["rows"] == 1000, data
assert data["generated"]["bytes"] > 0, data
assert data["totalMs"] > 0, data
for name in ("schema", "filter", "search", "quality"):
    assert any(item["name"] == name for item in data["commands"]), data
summary = json.load(open(sys.argv[1].replace(".bench.json", ".summary.json")))
assert summary["dataset"]["rows"] == 1000, summary
assert "path" not in summary["dataset"], summary
assert summary["totalMs"] > 0, summary
print("bench_smoke=json_ok")
PY
for name in a b; do
  CSVX_BIN="$BIN" python3 scripts/bench_100gb.py --rows 200 --out "$WORK/det-$name.csv" >/dev/null
  test -s "$WORK/det-$name.csv" || { echo "bench_smoke=fail determinism data" >&2; exit 1; }
done
cmp "$WORK/det-a.csv" "$WORK/det-b.csv" || { echo "bench_smoke=fail non-deterministic data" >&2; exit 1; }
echo "bench_smoke=deterministic"

if CSVX_BIN="$BIN" python3 scripts/bench_100gb.py --rows 10 --out "$WORK/nope.csv" --binary "$WORK/missing" >/dev/null 2>&1; then
  echo "bench_smoke=fail bad binary accepted" >&2
  exit 1
fi
echo "bench_smoke=ok"
