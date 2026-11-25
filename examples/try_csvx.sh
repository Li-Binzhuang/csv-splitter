#!/usr/bin/env bash
# Five-minute local tour: no network, no GUI, no model, temporary data only.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${CSVX_BIN:-$ROOT/csvx}
if [[ ! -x "$BIN" ]]; then
  echo "error: csvx binary not found at $BIN (set CSVX_BIN)" >&2
  exit 1
fi
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT
cat > "$DIR/orders.csv" <<'CSV'
id,date,city,amount
1,2026-10-20,CDMX,1200
2,2026-10-21,Puebla,20
3,2026-10-21,CDMX,30
CSV
echo "== version =="
"$BIN" --version
echo "== schema =="
"$BIN" schema "$DIR/orders.csv" --json
echo "== view =="
"$BIN" view "$DIR/orders.csv" --json --count 2
echo "== staged edit (journal) =="
"$BIN" edit set "$DIR/orders.csv" --row 2 --col city --value X --json
"$BIN" view "$DIR/orders.csv" --json --count 2
echo "== materialise =="
"$BIN" edit export "$DIR/orders.csv" -o "$DIR/clean.csv"
echo "== report =="
"$BIN" report "$DIR/clean.csv" --spec-text $'title,Five-minute tour\nrows,10\ndatecounts,date:city' --format md
echo "== xlsx =="
"$BIN" convert "$DIR/clean.csv" --format xlsx -o "$DIR/clean.xlsx" --highlight "amount>1000:red"
echo "== gate =="
"$BIN" gate "$DIR/clean.csv" --min-rows 3 --unique id --json
echo "== done =="
