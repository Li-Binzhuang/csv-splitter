#!/usr/bin/env bash
# End-to-end drill on a real file: index, open, jump, filter, search, and — with
# `--writes` — edit, export and verify. Each step prints one markdown table row with
# wall time and peak RSS, so the numbers in the README can be re-taken on any machine.
#
#   scripts/drill.sh path/to/file.csv              read path only
#   scripts/drill.sh path/to/file.csv --writes     add edit/export/verify
#   scripts/drill.sh path/to/file.csv --where 'amount>900' --jump 120000000
#
# `--writes` needs roughly twice the file size free (the export is a full copy).
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

BIN="${BIN:-target/release/csvx}"
if [ ! -x "$BIN" ]; then
  echo "build first: cargo build --release -p csvx-cli" >&2
  exit 2
fi

FILE=""
WRITES=0
WHERE="city=Tijuana"
JUMP="${JUMP:-1000000}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --writes) WRITES=1; shift ;;
    --where) WHERE="${2:?--where needs an expression}"; shift 2 ;;
    --jump) JUMP="${2:?--jump needs a row}"; shift 2 ;;
    --*) echo "unknown option $1" >&2; exit 2 ;;
    *) FILE="$1"; shift ;;
  esac
done
[ -n "$FILE" ] || { echo "usage: drill.sh <file.csv> [--writes] [--where EXPR] [--jump N]" >&2; exit 2; }
[ -f "$FILE" ] || { echo "no such file: $FILE" >&2; exit 2; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/csvx_drill.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

MB=$(( $(stat -f%z "$FILE" 2>/dev/null || stat -c%s "$FILE") / 1048576 ))
printf '\n### %s (%s MB)\n\n' "$(basename "$FILE")" "$MB"
printf '| step | wall time | peak RSS | note |\n|---|---|---|---|\n'

# One row per step. `time -l` is macOS; `time -v` is Linux. Anything else falls back
# to plain bash timing without RSS, which is still useful.
time_row() {
  label="$1"; note="$2"; shift 2
  if /usr/bin/time -l true >/dev/null 2>&1; then
    out=$(/usr/bin/time -l "$@" 2>&1 >/dev/null)
    secs=$(printf '%s\n' "$out" | awk '/^ *[0-9.]+ real/ {print $1}' | tail -1)
    rss=$(printf '%s\n' "$out" | awk '/maximum resident/ {printf "%.0f", $1/1048576}')
    printf '| %s | %s s | %s MB | %s |\n' "$label" "${secs:-?}" "${rss:-?}" "$note"
  elif /usr/bin/time -v true >/dev/null 2>&1; then
    out=$(/usr/bin/time -v "$@" 2>&1 >/dev/null)
    secs=$(printf '%s\n' "$out" | awk '/Elapsed \(wall clock\)/ {split($8,a,":"); printf "%.2f", a[1]*3600+a[2]*60+a[3]}')
    rss=$(printf '%s\n' "$out" | awk '/Maximum resident/ {printf "%.0f", $6/1024}')
    printf '| %s | %s s | %s MB | %s |\n' "$label" "${secs:-?}" "${rss:-?}" "$note"
  else
    start=$(date +%s%N)
    "$@" >/dev/null 2>&1
    done_ns=$(date +%s%N)
    printf '| %s | %s s | — | %s |\n' "$label" "$(( (done_ns - start) / 1000000 ))ms" "$note"
  fi
}

time_row "build the row index" "one pass, reused afterwards" "$BIN" index "$FILE"
time_row "open (first screen)" "reads a window only" "$BIN" preview "$FILE" --rows 20
time_row "jump to row $JUMP" "sparse index" "$BIN" extract "$FILE" --rows "$JUMP-$((JUMP + 4))"
time_row "filter $WHERE" "whole-table scan" "$BIN" filter "$FILE" --where "$WHERE"
time_row "search (no match)" "whole-table scan" "$BIN" search "$FILE" 'zzz-not-present'

if [ "$WRITES" = "1" ]; then
  time_row "edit one cell" "journal only; base file untouched" \
    "$BIN" edit set "$FILE" --row 1 --col city --value DRILL
  time_row "export the edited sheet" "full copy written" \
    "$BIN" edit export "$FILE" -o "$WORK/exported.csv"
  time_row "verify the export" "split + merge, byte-identical" \
    "$BIN" verify "$WORK/exported.csv" --splits 4 --quiet
else
  printf '| edit / export / verify | — | — | skipped; pass --writes (needs 2× the file size) |\n'
fi

printf '\n'
