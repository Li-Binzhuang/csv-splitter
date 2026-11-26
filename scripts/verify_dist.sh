#!/usr/bin/env bash
# Verify an offline release tarball: checksum, members, and the binary.
set -euo pipefail
cd "$(dirname "$0")/.."
TAR=${1:-}
if [[ -z "$TAR" || ! -f "$TAR" ]]; then
  echo "usage: bash scripts/verify_dist.sh <dist.tar.gz>" >&2
  exit 2
fi
if [[ ! -f "$TAR.sha256" ]]; then
  echo "missing checksum: $TAR.sha256" >&2
  exit 2
fi
VERIFY_RESULT="${TAR%.tar.gz}.verify.json"
START_MS=$(python3 -c 'import time; print(int(time.time()*1000))')
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
phase_start=$START_MS
CHECK=startup
write_failure() {
  code=$?
  set +e
  END_MS=$(python3 -c 'import time; print(int(time.time()*1000))')
  DURATION_MS=$((END_MS - START_MS))
  printf '{"verified":false,"failedAt":"%s","error":"exit %s","durationMs":%s}\n' "$CHECK" "$code" "$DURATION_MS" > "$VERIFY_RESULT"
  echo "verification failed at $CHECK" >&2
  exit "$code"
}
trap write_failure ERR
on_exit() {
  code=$?
  if [[ $code -ne 0 && ! -f "$VERIFY_RESULT" ]]; then
    set +e
    END_MS=$(python3 -c 'import time; print(int(time.time()*1000))')
    DURATION_MS=$((END_MS - START_MS))
    printf '{"verified":false,"failedAt":"%s","error":"exit %s","durationMs":%s}\n' "$CHECK" "$code" "$DURATION_MS" > "$VERIFY_RESULT"
    echo "verification failed at $CHECK" >&2
  fi
  [[ -n "${AGENT_PID:-}" ]] && kill "$AGENT_PID" 2>/dev/null || true
  [[ -n "${STAGE:-}" ]] && rm -rf "$STAGE"
  [[ -n "${AGENT_DIR:-}" ]] && rm -rf "$AGENT_DIR"
}
trap on_exit EXIT
CHECK=checksum
shasum -a 256 -c "$TAR.sha256"
STAGE=$(mktemp -d)
tar -xzf "$TAR" -C "$STAGE"
ROOT=$(find "$STAGE" -maxdepth 1 -mindepth 1 -type d | head -1)
EXTRACT_MS=$(( $(now_ms) - phase_start ))
phase_start=$(now_ms)
CHECK=members
for required in csvx README.md AGENT_QUICKSTART.md TRY_CSVX.sh docs/RELEASE_CHECKLIST.md docs/ROADMAP.md docs/NIGHT_CHECKPOINT.md docs/ENTRYPOINTS.md docs/SECURITY.md docs/DESIGN_PARTNER.md docs/SUPPORT_MATRIX.md docs/DEPLOY.md docs/MIGRATION.md docs/PILOT_INTERVIEW.md docs/bench-100gb-2026-11-26.json mcp_sdk/client.mjs examples/agent/http_session.sh examples/try_csvx.sh scripts/agent_routes_smoke.py scripts/install_offline.sh scripts/install_selfcheck.sh scripts/test_upgrade_rollback.sh scripts/ops_status.sh scripts/ops_bundle.sh scripts/bench_100gb.py scripts/pilot_kit.sh scripts/pilot_intake.py skills/csvx-data/SKILL.md; do
  if [[ ! -f "$ROOT/$required" ]]; then
    echo "missing member: $required" >&2
    exit 1
  fi
done
CHECK=version
"$ROOT/csvx" --version
CHECK=skill
for term in 'csvx-data-workbench' 'csvx_schema' 'approval' 'audit'; do
  grep -q "$term" "$ROOT/skills/csvx-data/SKILL.md" || {
    echo "skill missing: $term" >&2
    exit 1
  }
done
printf 'skill=ok (surface/approval/audit)\n'
CHECK=skill_install
DSH_TMP=$(mktemp -d)
DSH_HOME="$DSH_TMP" bash "$ROOT/install_skill.sh" >/dev/null
test -f "$DSH_TMP/skills/csvx-data/SKILL.md"
grep -q 'name: csvx-data-workbench' "$DSH_TMP/skills/csvx-data/SKILL.md"
printf 'skill_install=ok\n'
rm -rf "$DSH_TMP"
SKILL_MS=$(( $(now_ms) - phase_start ))
phase_start=$(now_ms)
CHECK=try
phase_start=$(now_ms)
CSVX_BIN="$ROOT/csvx" bash "$ROOT/TRY_CSVX.sh" >/dev/null
TRY_MS=$(( $(now_ms) - phase_start ))

CHECK=agent
phase_start=$(now_ms)
AGENT_DIR=$(mktemp -d)
printf 'id,city,amount\n1,CDMX,10\n2,Puebla,20\n' > "$AGENT_DIR/orders.csv"
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
"$ROOT/csvx" agent serve --root "$AGENT_DIR" --addr "127.0.0.1:$PORT" >"$AGENT_DIR/server.log" 2>&1 &
AGENT_PID=$!

for _ in $(seq 1 50); do
  if python3 -c "import urllib.request,sys; urllib.request.urlopen('http://127.0.0.1:$PORT/agent/v1/manifest', timeout=1)" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
AGENT_OUT=$(python3 "$ROOT/scripts/agent_routes_smoke.py" --api "http://127.0.0.1:$PORT" --file orders.csv --binary "$ROOT/csvx" --root "$AGENT_DIR")
printf '%s\n' "$AGENT_OUT"
printf 'stats: %s\n' "$AGENT_OUT"
STATS_JSON=$(printf '%s\n' "$AGENT_OUT" | sed -n 's/^stats_json=//p' | tail -1)
CHECK=stats
python3 "$ROOT/scripts/check_agent_stats.py" "$STATS_JSON"
kill "$AGENT_PID" 2>/dev/null || true
wait "$AGENT_PID" 2>/dev/null || true

AGENT_MS=$(( $(now_ms) - phase_start ))
CHECK=xlsx
phase_start=$(now_ms)
XLSX_DIR=$(mktemp -d)
printf 'date,city,amount\n2026-10-20,CDMX,1200\n2026-10-21,Puebla,20\n' > "$XLSX_DIR/x.csv"
"$ROOT/csvx" convert "$XLSX_DIR/x.csv" --format xlsx -o "$XLSX_DIR/x.xlsx" --highlight "amount>1000:red"
python3 - "$XLSX_DIR/x.xlsx" <<'PY'
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1]) as archive:
    names = archive.namelist()
    sheet = archive.read("xl/worksheets/sheet1.xml").decode()
if "xl/worksheets/sheet1.xml" not in names:
    raise SystemExit(f"xlsx missing sheet1: {names}")
if "conditionalFormatting" not in sheet or "cellIs" not in sheet:
    raise SystemExit("xlsx conditional formatting missing")
print("xlsx=ok")
PY
rm -rf "$XLSX_DIR"

XLSX_MS=$(( $(now_ms) - phase_start ))
CHECK=trace
phase_start=$(now_ms)
TRACE_DIR=$(mktemp -d)
printf 'id,city,amount\n1,A,10\n2,B,20\n' > "$TRACE_DIR/t.csv"
cat > "$TRACE_DIR/trace.jsonl" <<'JSONL'
{"event":"tool","tool":"csvx_schema","arguments":{"path":"t.csv"}}
{"event":"tool","tool":"csvx_edit_apply","arguments":{"path":"t.csv","ops":"set:1:city:X"}}
JSONL
TRACE_OUT=$("$ROOT/csvx" agent replay --root "$TRACE_DIR" --replay "$TRACE_DIR/trace.jsonl" 2>&1)
TRACE_LINES=$(printf '%s\n' "$TRACE_OUT" | grep -c '^replay ' || true)
test "$TRACE_LINES" -eq 2
"$ROOT/csvx" view "$TRACE_DIR/t.csv" --json | grep -q '"X"'
printf 'trace_replay=tools:%s\n' "$TRACE_LINES"
rm -rf "$TRACE_DIR"
TRACE_MS=$(( $(now_ms) - phase_start ))

END_MS=$(python3 -c 'import time; print(int(time.time()*1000))')
DURATION_MS=$((END_MS - START_MS))
TMP_RESULT="${VERIFY_RESULT}.tmp"
cat > "$TMP_RESULT" <<EOF
{"verified":true,"failedAt":null,"durationMs":$DURATION_MS,"phaseMs":{"extract":$EXTRACT_MS,"skill":$SKILL_MS,"try":$TRY_MS,"agent":$AGENT_MS,"xlsx":$XLSX_MS,"trace":$TRACE_MS},"phaseThresholds":"ok","checksum":"ok","members":"ok","skill":"ok","skillInstall":"ok","xlsx":"ok","traceReplay":$TRACE_LINES,"stats":$STATS_JSON}
EOF
CHECK=phases
python3 "$ROOT/scripts/check_phase_times.py" "$TMP_RESULT"
mv "$TMP_RESULT" "$VERIFY_RESULT"
echo "verified: $TAR"
