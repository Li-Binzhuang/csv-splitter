#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
BAD='{"verified":true,"durationMs":9999,"phaseMs":{"extract":10,"skill":10,"try":10,"agent":9999,"xlsx":10,"trace":10}}'
if python3 scripts/check_phase_times.py <(printf '%s' "$BAD") >/tmp/csvx_phase_bad.log 2>&1; then
  echo "error: bad phases accepted" >&2
  exit 1
fi
grep -q 'phase threshold mismatch' /tmp/csvx_phase_bad.log
grep -q 'agent=9999ms' /tmp/csvx_phase_bad.log
rm -f /tmp/csvx_phase_bad.log
echo "phase negative=ok"
