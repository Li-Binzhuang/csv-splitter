#!/usr/bin/env bash
# Reproduce the xlsx round-trip benchmark: CSV -> xlsx -> CSV, compare bytes.
#
#   scripts/bench_xlsx_roundtrip.sh [rows]
#
# Build the release binary first: cargo build --release -p csvx-cli
set -euo pipefail
cd "$(dirname "$0")/.."

rows="${1:-200000}"
bin="${CSVX_BIN:-target/release/csvx}"
if [ ! -x "$bin" ]; then
  echo "missing $bin; run: cargo build --release -p csvx-cli" >&2
  exit 2
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

LC_ALL=C awk -v rows="$rows" 'BEGIN {
  print "id,city,amount,qty,flag,a,b,c,d,e";
  for (i = 1; i <= rows; i++)
    printf "%d,city%d,%d,%d,%d,%d,%d,%d,%d,%d\n", i, i % 100, i * 7 % 10000, i % 50, i % 2, i % 1000, i % 100, i % 10, i % 3, i % 5;
}' > "$tmp/in.csv"

echo "rows=$rows input=$(wc -c < "$tmp/in.csv" | tr -d ' ') bytes"
/usr/bin/time -p "$bin" convert "$tmp/in.csv" --format xlsx --out "$tmp/book.xlsx" --quiet
echo "xlsx=$(wc -c < "$tmp/book.xlsx" | tr -d ' ') bytes"
/usr/bin/time -p "$bin" convert "$tmp/book.xlsx" --format csv --out "$tmp/out.csv" --quiet

if cmp -s "$tmp/in.csv" "$tmp/out.csv"; then
  echo "roundtrip=identical"
else
  echo "roundtrip=DIFFERENT" >&2
  exit 1
fi
