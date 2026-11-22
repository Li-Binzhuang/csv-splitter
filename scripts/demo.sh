#!/usr/bin/env bash
# One command that shows what csvx can do, on data it generates itself.
#
#   scripts/demo.sh                          feature tour on `csvx demo` data
#   scripts/demo.sh --big file.csv           … then the big-file drill on that file
#   scripts/demo.sh --big file.csv --writes  … including edit/export/verify (2× disk)
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
BIG_WRITES=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --big) BIG="${2:?--big needs a file}"; shift 2 ;;
    --writes) BIG_WRITES=1; shift ;;
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
run "$BIN" edit formula orders.csv --name big_orders --value "countif(amount, \">1000\")"
run "$BIN" edit formula orders.csv --name min_sale --value "min(amount)"
run "$BIN" edit formula orders.csv --name max_sale --value "max(amount)"
run "$BIN" edit formula orders.csv --name next_month --value "edate(ts, 1)"
run "$BIN" edit formula orders.csv --name city_with_m --value "countif(city, \"*M*\")"

step "5. conditional formatting and layout, as data a front-end can render"
run "$BIN" format orders.csv --rule "amount>1000:bg=red" --summary
run "$BIN" layout orders.csv --freeze-first 2 --set "amount:width=120"

step "4b. a transaction of several ops, taken back by one undo"
run "$BIN" edit transaction orders.csv --ops "set:1:note:VIP2;delete:2:1"
run "$BIN" edit undo orders.csv

step "6. aggregate, pivot, chart (same numbers through native or DuckDB)"
run "$BIN" groupby orders.csv --by city --agg "amount:sum,amount:count" --order sum_amount --desc
run "$BIN" pivot orders.csv --rows city --values "amount:sum"
run "$BIN" chart orders.csv --x city --y "amount:sum" --type bar

step "7. export the edited sheet and prove it is readable"
run "$BIN" edit export orders.csv -o clean.csv
printf '  -> clean.csv: %s bytes (quoted newlines make wc -l an unreliable row count)\n' \
  "$(wc -c < clean.csv | tr -d ' ')"
printf '  -> columns: %s\n' "$(head -1 clean.csv)"

step "8. the HTTP backend starts and says everything is configured"
# 8787 is the documented default, but a live demo must not fail because something else
# already holds it. Ask the OS for a free port and let --check prove the rest.
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()' 2>/dev/null || echo 8787)
run "$BIN" serve --check --root . --port "$PORT"

if [ -n "$BIG" ]; then
  step "9. the same product on a real big file ($(basename "$BIG"))"
  extra=""
  [ "$BIG_WRITES" = "1" ] && extra="--writes"
  bash "$OLDPWD/scripts/drill.sh" "$BIG" $extra
fi

printf '\nDemo finished. Working copy was %s (removed on exit).\n' "$WORK"
