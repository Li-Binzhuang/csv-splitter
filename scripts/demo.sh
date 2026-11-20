#!/usr/bin/env bash
# One command that shows what csvx can do, on data it generates itself.
#
#   scripts/demo.sh                      feature tour on `csvx demo` data
#   scripts/demo.sh --big file.csv       … then the big-file drill on that file
#
# Every step prints the command it runs, so the output doubles as a script a person
# can follow by hand. Nothing here needs the network, a database, or a GUI.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

BIN="${BIN:-target/release/csvx}"
if [ ! -x "$BIN" ]; then
  echo "build first: cargo build --release -p csvx-cli" >&2
  exit 2
fi
# Absolute, because the tour changes directory into the temp workdir.
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

BIG=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --big) BIG="${2:?--big needs a file}"; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

if [ -n "$BIG" ]; then
  [ -f "$BIG" ] || { echo "no such file: $BIG" >&2; exit 2; }
  BIG="$(cd "$(dirname "$BIG")" && pwd)/$(basename "$BIG")"
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/csvx_demo.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

step() { printf '\n== %s ==\n' "$1"; }
run() {
  printf '$ %s\n' "$*"
  "$@" 2>&1 | head -12
}

step "0. generate a deliberately dirty sample"
run "$BIN" demo -o .
printf '  -> %s\n' "$(ls -1)"

step "1. open and look (index built once, constant memory)"
run "$BIN" preview orders.csv --rows 4

step "2. profile every column from one pass"
run "$BIN" analyze orders.csv

step "3. filter, and page the hits with row numbers"
run "$BIN" filter orders.csv --where "city=CDMX" --limit 3 --show

step "4. edit without touching the file: a cell and a computed column"
run "$BIN" edit set orders.csv --row 1 --col note --value "VIP"
run "$BIN" edit formula orders.csv --name vat --value "amount*0.16"
run "$BIN" edit formula orders.csv --name all_sales --value "sum(amount)"

step "5. conditional formatting and layout, as data a front-end can render"
run "$BIN" format orders.csv --rule "amount>1000:bg=red" --summary
run "$BIN" layout orders.csv --freeze-first 2 --set "amount:width=120"

step "6. aggregate, pivot, chart (same numbers through native or DuckDB)"
run "$BIN" groupby orders.csv --by city --agg "amount:sum,amount:count" --order sum_amount --desc
run "$BIN" pivot orders.csv --rows city --values "amount:sum"
run "$BIN" chart orders.csv --x city --y "amount:sum" --type bar

step "7. export the edited sheet and prove it is readable"
run "$BIN" edit export orders.csv -o clean.csv
printf '  -> clean.csv: %s bytes (quoted newlines make wc -l an unreliable row count)\n' \
  "$(wc -c < clean.csv | tr -d ' ')"

step "8. the HTTP backend starts and says everything is configured"
run "$BIN" serve --check --root .

if [ -n "$BIG" ]; then
  step "9. the same product on a real big file ($(basename "$BIG"))"
  bash "$OLDPWD/scripts/drill.sh" "$BIG"
fi

printf '\nDemo finished. Working copy was %s (removed on exit).\n' "$WORK"
