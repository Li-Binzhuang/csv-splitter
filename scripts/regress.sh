#!/usr/bin/env bash
# Every command once, on a realistic file, checking that it either succeeds or refuses
# with a reason. `smoke.sh` covers the main paths in detail; this covers the surface.
set -u
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/debug/csv-splitter}"
if [ ! -x "$BIN" ]; then
  echo "build the binary first: cargo build -p csv-splitter-cli" >&2
  exit 2
fi
# Absolute, because the script changes directory before running anything.
case "$BIN" in
  /*) ;;
  *) BIN="$(pwd)/${BIN#./}" ;;
esac
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
"$BIN" demo -o . >/dev/null 2>&1 || { echo "demo failed" >&2; exit 1; }
printf 'a,b\n1,2\n' > small.csv

failed=0
check() {
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    printf '  ok   %s\n' "$desc"
  else
    printf '  FAIL %s\n' "$desc"
    failed=$((failed + 1))
  fi
}

check "preview"    "$BIN" preview orders.csv --rows 3
check "view"       "$BIN" view orders.csv --count 3
check "index"      "$BIN" index orders.csv
check "filter"     "$BIN" filter orders.csv --where "city=CDMX" -o f.csv
check "search"     "$BIN" search orders.csv CDMX --limit 3
check "stats"      "$BIN" stats orders.csv --sample 1000
check "analyze"    "$BIN" analyze orders.csv --sample 1000
check "validate"   "$BIN" validate orders.csv --check "city:not-empty"
check "dedupe"     "$BIN" dedupe orders.csv --by cust_no -o d.csv
check "columns"    "$BIN" columns orders.csv --keep id,city -o c.csv
check "schema"      "$BIN" schema orders.csv --sample 5
check "schema --has" "$BIN" schema orders.csv --has city
check "expect"      "$BIN" edit set orders.csv --row 1 --col city --expect "$(head -2 orders.csv | tail -1 | cut -d, -f3)" --value Z
check "concat"     "$BIN" concat small.csv small.csv -o cc.csv
check "partition"  "$BIN" partition orders.csv --by city -o parts.tar
check "sort"       "$BIN" sort orders.csv --by amount -o s.csv
check "groupby"    "$BIN" groupby orders.csv --by city --agg "amount:sum" -o g.csv
check "pivot"      "$BIN" pivot orders.csv --rows city --cols note --values "amount:sum"
check "chart"      "$BIN" chart orders.csv --x city --y "amount:sum"
check "lookup"     "$BIN" lookup orders.csv --key cust_no --from orders.csv --get amount -o l.csv
check "sql"        "$BIN" sql orders.csv "SELECT count(*) FROM t"
check "format"     "$BIN" format orders.csv --rule "amount>50000:bg=red" --summary
check "layout"     "$BIN" layout orders.csv --set "amount:width=120"
check "book"       "$BIN" book --root .
check "files"      "$BIN" files --root .
check "split"      "$BIN" split orders.csv -n 2 -o sp.tar
check "merge"      "$BIN" merge sp.tar -o m.csv
check "verify"     "$BIN" verify orders.csv -n 2
check "extract"    "$BIN" extract sp.tar -o ex/
check "convert"    "$BIN" convert orders.csv --to utf-8 -o u.csv
check "edit set"   "$BIN" edit set orders.csv --row 1 --col city --value X
check "edit fill"  "$BIN" edit fill orders.csv --col amount --from-row 2 --to-row 5 --value 0
check "edit formula" "$BIN" edit formula orders.csv --name vat --value "amount*0.16"
check "edit export" "$BIN" edit export orders.csv --columns "id,city" --order "amount:desc" -o e2.csv
check "groupby + edits" "$BIN" groupby orders.csv --by city --agg "vat:sum" --apply-edits -o g2.csv
check "edit undo"  "$BIN" edit undo orders.csv

if [ "$failed" -ne 0 ]; then
  echo "== $failed command(s) failed =="
  exit 1
fi
echo "== all commands answered =="
