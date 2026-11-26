#!/usr/bin/env bash
# Smoke benchmark summary comparison.
set -euo pipefail
cd "$(dirname "$0")/.."
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
python3 - "$WORK/base.json" "$WORK/cand.json" "$WORK/slow.json" "$WORK/missing.json" "$WORK/total.json" "$WORK/dataset.json" <<'PY'
import json
import sys
names = ("schema", "filter", "search", "quality")
def write(path, elapsed, total, dataset=None):
    payload = {
        "commands": [{"name": n, "elapsedMs": elapsed[i], "rssBytes": 1000, "exit": 0} for i, n in enumerate(names)],
        "totalMs": total,
    }
    if dataset:
        payload["dataset"] = dataset
    json.dump(payload, open(path, "w"))
write(sys.argv[1], [100, 200, 300, 400], 1000, {"bytes": 1000, "rows": 100})
write(sys.argv[2], [110, 210, 310, 410], 1040, {"bytes": 1000, "rows": 100})
write(sys.argv[3], [110, 260, 310, 410], 1090, {"bytes": 1000, "rows": 100})
data = json.load(open(sys.argv[3]))
data["commands"] = data["commands"][:3]
json.dump(data, open(sys.argv[4], "w"))
write(sys.argv[5], [110, 210, 310, 410], 1400, {"bytes": 1000, "rows": 100})
write(sys.argv[6], [110, 210, 310, 410], 1040, {"bytes": 2000, "rows": 100})
PY
python3 scripts/bench_compare.py --json "$WORK/base.json" "$WORK/cand.json" > "$WORK/ok.json"
python3 - "$WORK/ok.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["ok"] is True, data
assert data["totalDeltaPercent"] == 4.0, data
print("bench_compare=ok")
PY
if python3 scripts/bench_compare.py --json "$WORK/base.json" "$WORK/slow.json" > "$WORK/slow.out"; then
  echo "bench_compare=fail regression accepted" >&2
  exit 1
fi
python3 - "$WORK/slow.out" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["ok"] is False, data
assert data["regressions"][0]["name"] == "filter", data
print("bench_compare=regression_rejected")
PY
if python3 scripts/bench_compare.py --json "$WORK/base.json" "$WORK/total.json" >"$WORK/total.out" 2>&1; then
  echo "bench_compare=fail total regression accepted" >&2
  exit 1
fi
python3 - "$WORK/total.out" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["ok"] is False, data
assert data["totalRegression"] is True, data
print("bench_compare=total_regression_rejected")
PY
if python3 scripts/bench_compare.py --json "$WORK/base.json" "$WORK/dataset.json" >"$WORK/dataset.out" 2>&1; then
  echo "bench_compare=fail dataset mismatch accepted" >&2
  exit 1
fi
python3 - "$WORK/dataset.out" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["ok"] is False, data
assert data["datasetMismatch"], data
print("bench_compare=dataset_mismatch_rejected")
PY
if python3 scripts/bench_compare.py --json "$WORK/base.json" "$WORK/missing.json" >/dev/null 2>&1; then
  echo "bench_compare=fail missing command accepted" >&2
  exit 1
fi
echo "bench_compare=ok"
