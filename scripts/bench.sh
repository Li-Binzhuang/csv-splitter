#!/usr/bin/env bash
# Re-measure the numbers the README claims, on a file you choose.
#
#   scripts/bench.sh path/to/file.csv [rows-to-generate]
#
# With no argument it generates a 5 M row / ~230 MB fixture, measures the
# documented scenarios, prints a markdown table, and deletes the fixture.
#
# Memory is measured as peak RSS via /usr/bin/time -l on macOS or /usr/bin/time -v
# elsewhere. The point of the table is the *shape*: time grows with bytes, memory
# should not.
set -uo pipefail
cd "$(dirname "$0")/.."

export PATH="$HOME/.cargo/bin:$PATH"
BIN="$(pwd)/target/release/csv-splitter"
if [ ! -x "$BIN" ]; then
  echo "building release (one time)"
  cargo build --release -p csv-splitter-cli -q || exit 1
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/csvs_bench.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

FILE="${1:-}"
if [ -z "$FILE" ]; then
  ROWS="${2:-5000000}"
  echo "generating $ROWS rows"
  python3 - "$WORK/big.csv" "$ROWS" <<'PY'
import random, sys
path, rows = sys.argv[1], int(sys.argv[2])
choices = ["CDMX", "Puebla", "Tijuana", "Monterrey"]
with open(path, "w") as f:
    f.write("id,city,amount,note,ts\n")
    for i in range(rows):
        f.write(f"{i},{choices[i % 4]},{i % 997},note text here,2024-03-15\n")
PY
  FILE="$WORK/big.csv"
fi
SIZE=$(stat -f%z "$FILE" 2>/dev/null || stat -c%s "$FILE")
MB=$((SIZE / 1048576))

peak() {
  if /usr/bin/time -l true >/dev/null 2>&1; then
    /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/maximum resident/ {printf "%.1f", $1/1048576}'
  else
    /usr/bin/time -v "$@" 2>&1 >/dev/null | awk '/Maximum resident/ {printf "%.1f", $3/1024}'
  fi
}
# Some commands animate a progress bar on stdout, which glues `real` to it — look
# for the token anywhere in the stream, not at the start of a line.
secs() {
  /usr/bin/time -p "$@" 2>&1 >/dev/null \
    | tr '\r' '\n' \
    | grep -oE 'real [0-9.]+' \
    | tail -1 \
    | awk '{printf "%.2f", $2}'
}
# Some commands write their summary to stdout; do not discard the timing with it.


printf '\n### %s (%s MB)\n\n' "$(basename "$FILE")" "$MB"
printf '| scenario | time | peak memory | note |\n|---|---|---|---|\n'
printf '| build the row index | %s s | %s MB | %s bytes |\n' \
  "$(secs "$BIN" index "$FILE")" "$(peak "$BIN" index "$FILE")" \
  "$(stat -f%z "$FILE.idx" 2>/dev/null || stat -c%s "$FILE.idx" 2>/dev/null || echo 0)"
printf '| jump to the middle | %s s | %s MB | sparse index |\n' \
  "$(secs "$BIN" extract "$FILE" --rows 1000000-1000002)" \
  "$(peak "$BIN" extract "$FILE" --rows 1000000-1000002)"
printf '| preview | %s s | %s MB | reads a window only |\n' \
  "$(secs "$BIN" preview "$FILE" --rows 20)" "$(peak "$BIN" preview "$FILE" --rows 20)"
printf '| analyze | %s s | %s MB | one full pass |\n' \
  "$(secs "$BIN" analyze "$FILE")" "$(peak "$BIN" analyze "$FILE")"
printf '| split into 8 | %s s | %s MB | |\n' \
  "$(secs "$BIN" split "$FILE" --splits 8 -o "$WORK/s1.tar")" \
  "$(peak "$BIN" split "$FILE" --splits 8 -o "$WORK/s2.tar")"
printf '| split into 8, 4 threads | %s s | %s MB | |\n' \
  "$(secs "$BIN" split "$FILE" --splits 8 --threads 4 -o "$WORK/s3.tar")" \
  "$(peak "$BIN" split "$FILE" --splits 8 --threads 4 -o "$WORK/s4.tar")"
printf '| merge back | %s s | %s MB | |\n' \
  "$(secs "$BIN" merge "$WORK/s1.tar" -o "$WORK/m1.csv")" \
  "$(peak "$BIN" merge "$WORK/s1.tar" -o "$WORK/m2.csv")"
printf '| groupby (4 groups) | %s s | %s MB | |\n' \
  "$(secs "$BIN" groupby "$FILE" --by city --agg "amount:sum" -o "$WORK/g.csv")" \
  "$(peak "$BIN" groupby "$FILE" --by city --agg "amount:sum" -o "$WORK/g2.csv")"
printf '\nMemory should stay flat as the file grows; time should scale with bytes.\n'
