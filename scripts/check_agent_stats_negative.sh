#!/usr/bin/env bash
# Prove the stats checker rejects out-of-range values with a named field.
set -euo pipefail
cd "$(dirname "$0")/.."
BAD='{"routes":11,"session":"bad","actions":2,"sse_ms":900,"deny":403,"replay":2}'
if python3 scripts/check_agent_stats.py "$BAD" >/tmp/csvx_stats_bad.log 2>&1; then
  echo "error: bad stats were accepted" >&2
  exit 1
fi
grep -q 'threshold mismatch' /tmp/csvx_stats_bad.log
grep -q 'routes=11' /tmp/csvx_stats_bad.log
grep -q 'sse_ms=900' /tmp/csvx_stats_bad.log
rm -f /tmp/csvx_stats_bad.log
echo "stats negative=ok"
