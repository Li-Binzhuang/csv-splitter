#!/usr/bin/env bash
# Replay a CLI trace without a model or API key.
set -euo pipefail
trace=${1:?usage: trace_replay.sh TRACE.jsonl [ROOT]}
root=${2:-$PWD}
csvx=${CSVX_BIN:-./target/release/csvx}
"$csvx" agent replay --root "$root" --replay "$trace"
