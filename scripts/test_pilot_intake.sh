#!/usr/bin/env bash
# Smoke the design-partner intake validator: naming warnings, size refusal, no raw values.
set -euo pipefail
cd "$(dirname "$0")/.."
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
printf 'id,email,phone,amount\n1,person@example.test,5550100,10\n' > "$WORK/sample.csv"
python3 scripts/pilot_intake.py --sample "$WORK/sample.csv" --out "$WORK/good" >/dev/null
test -s "$WORK/good/intake.json" || { echo "pilot_intake_smoke=fail json" >&2; exit 1; }
test -s "$WORK/good/intake.md" || { echo "pilot_intake_smoke=fail md" >&2; exit 1; }
python3 - "$WORK/good/intake.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["columnCount"] == 4, data
assert data["readRawRows"] is False, data
kinds = {item["kind"] for item in data["sensitiveWarnings"]}
assert {"email", "phone"} <= kinds, data
raw = open(sys.argv[1]).read() + open(sys.argv[1].replace("intake.json", "intake.md")).read()
assert "person@example.test" not in raw
print("pilot_intake_smoke=json_ok")
PY

gzip -c "$WORK/sample.csv" > "$WORK/sample.csv.gz"
python3 scripts/pilot_intake.py --sample "$WORK/sample.csv.gz" --out "$WORK/gz" >/dev/null
python3 - "$WORK/gz/intake.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["columnCount"] == 4 and not data["error"], data
PY
bzip2 -c "$WORK/sample.csv" > "$WORK/sample.csv.bz2"
python3 scripts/pilot_intake.py --sample "$WORK/sample.csv.bz2" --out "$WORK/bz2" >/dev/null
python3 - "$WORK/bz2/intake.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["columnCount"] == 4 and not data["error"], data
PY
if command -v zstd >/dev/null 2>&1; then
  zstd -q -c "$WORK/sample.csv" > "$WORK/sample.csv.zst"
  python3 scripts/pilot_intake.py --sample "$WORK/sample.csv.zst" --out "$WORK/zst" >/dev/null
  python3 - "$WORK/zst/intake.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["columnCount"] == 4 and not data["error"], data
PY
fi

{
  printf 'id,note\n'
  for i in $(seq 1 100000); do printf '%s,xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n' "$i"; done
} > "$WORK/big.csv"
set +e
python3 scripts/pilot_intake.py --sample "$WORK/big.csv" --out "$WORK/big" --max-gb 0.001 >/dev/null 2>&1
code=$?
set -e
[[ "$code" -eq 1 ]] || { echo "pilot_intake_smoke=fail oversize code=$code" >&2; exit 1; }
python3 - "$WORK/big/intake.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["withinLimit"] is False, data
PY

printf 'x%.0s' $(seq 1 300000) > "$WORK/malformed.csv"
set +e
python3 scripts/pilot_intake.py --sample "$WORK/malformed.csv" --out "$WORK/malformed" >/dev/null 2>&1
code=$?
set -e
[[ "$code" -eq 2 ]] || { echo "pilot_intake_smoke=fail malformed code=$code" >&2; exit 1; }
python3 - "$WORK/malformed/intake.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["error"], data
PY

set +e
python3 scripts/pilot_intake.py --sample "$WORK/missing.csv" --out "$WORK/missing" >/dev/null 2>&1
code=$?
set -e
[[ "$code" -eq 2 ]] || { echo "pilot_intake_smoke=fail missing code=$code" >&2; exit 1; }
echo "pilot_intake_smoke=ok"
