#!/usr/bin/env bash
# Run the gate and keep its exit status, which a pipeline would eat.
#
# `bash scripts/dev.sh gate | tail` reports tail's status, not the gate's: a failing clippy
# run then looks like success to anything chained with `&&`. This wrapper captures the
# status, shows the tail, and prints it, so a caller can trust `$?`.
#
#   scripts/gate.sh            run the gate
#   GATE_FAIL_TEST=1 scripts/gate.sh   self-test: exits 3 without compiling anything
set -uo pipefail
cd "$(dirname "$0")/.."

if [ "${GATE_FAIL_TEST:-0}" = "1" ]; then
  # Run the real gate with the injected failing stage: the point is to prove the gate names
  # the stage that failed, not merely that some wrapper can exit 3.
  log=$(mktemp)
  bash scripts/dev.sh gate >"$log" 2>&1
  code=$?
  grep -E "== stage (selftest) failed ==" "$log" | head -1
  rm -f "$log"
  echo "== gate exit=$code =="
  exit "$code"
fi

log=$(mktemp)
bash scripts/dev.sh gate >"$log" 2>&1
code=$?
if [ "$code" -ne 0 ]; then
  grep -E "^error|FAILED|panicked|Diff in" "$log" | head -20 || true
fi
tail -n 5 "$log"
rm -f "$log"
echo "== gate exit=$code =="
exit "$code"
