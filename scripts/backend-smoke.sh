#!/usr/bin/env bash
# End-to-end check of the HTTP backend: start it, exercise the framework surface,
# assert the fields a front-end depends on.
#
# This exists because cargo tests call handlers directly; only this checks the thing a
# client actually talks to — one server process, real requests, real JSON.
set -uo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/debug/csv-splitter}"
[ -x "$BIN" ] || { echo "build first: cargo build -p csv-splitter-cli" >&2; exit 2; }
case "$BIN" in /*) ;; *) BIN="$(pwd)/${BIN#./}" ;; esac

WORK="$(mktemp -d)"
trap 'kill "${PID:-}" 2>/dev/null; rm -rf "$WORK"' EXIT
cd "$WORK"

"$BIN" demo -o . >/dev/null 2>&1 || { echo "demo failed" >&2; exit 1; }

"$BIN" serve --root "$WORK" --addr 127.0.0.1:0 > banner.txt 2>&1 &
PID=$!
for _ in $(seq 1 40); do grep -q "http://" banner.txt 2>/dev/null && break; sleep 0.2; done
URL="$(grep -oE 'http://127\.0\.0\.1:[0-9]+' banner.txt | head -1)"
[ -n "$URL" ] || { echo "the server never printed its address" >&2; cat banner.txt; exit 1; }

failed=0
has() { # has <description> <json> <substring>
  if printf '%s' "$2" | grep -qF "$3"; then
    printf '  ok   %s\n' "$1"
  else
    printf '  FAIL %s (want %s)\n' "$1" "$3"
    printf '       got: %s\n' "$(printf '%s' "$2" | cut -c1-160)"
    failed=$((failed + 1))
  fi
}
want_code() { # want_code <description> <expected-code> <path>
  local got
  got="$(curl -s -o /dev/null -w '%{http_code}' "$URL$3")"
  if [ "$got" = "$2" ]; then printf '  ok   %s (%s)\n' "$1" "$got"
  else printf '  FAIL %s (want %s got %s)\n' "$1" "$2" "$got"; failed=$((failed + 1)); fi
}
want_code_post() { # want_code_post <description> <expected-code> <path>
  local got
  got="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$URL$3")"
  if [ "$got" = "$2" ]; then printf '  ok   %s (%s)\n' "$1" "$got"
  else printf '  FAIL %s (want %s got %s)\n' "$1" "$2" "$got"; failed=$((failed + 1)); fi
}

echo "== framework surface =="
CAPS="$(curl -s "$URL/capabilities")"
has "capabilities list reads"    "$CAPS" '"/rows"'
has "capabilities list writes"   "$CAPS" '"POST /edit"'
has "capabilities list actions"  "$CAPS" '"set"'
has "capabilities query timeout" "$CAPS" '"querySeconds":30'

COLS="$(curl -s "$URL/columns?path=orders.csv&sample=10")"
has "columns: full count"        "$COLS" '"count":6'
has "columns: kind field"        "$COLS" '"kind":"'
has "columns: distinct field"    "$COLS" '"distinct":'
has "columns: 1-based index"     "$COLS" '"index":1'

EXISTS="$(curl -s "$URL/columns?path=orders.csv&name=city")"
has "column exists: true"        "$EXISTS" '"exists":true'
has "column exists: index"       "$EXISTS" '"index":3'

NOEXIST="$(curl -s "$URL/columns?path=orders.csv&name=nosuch")"
has "column exists: false"       "$NOEXIST" '"exists":false'

ROW1="$(curl -s "$URL/rows?path=orders.csv&start=0&count=2")"
has "rows: window contract"      "$ROW1" '"requested":2'
has "rows: total"                "$ROW1" '"total":'
has "rows: rowKeys"              "$ROW1" '"rowKeys":["b:0","b:1"]'

CAPPED="$(curl -s "$URL/rows?path=orders.csv&count=5000")"
has "cap: count is capped"       "$CAPPED" '"count":2000'
has "cap: requested is echoed"   "$CAPPED" '"requested":5000'
has "cap: limit is stated"       "$CAPPED" '"limit":2000'

NARROW="$(curl -s "$URL/rows?path=orders.csv&count=1&columns=id,city")"
has "rows: columns projection"   "$NARROW" '"columns":["id","city"]'

SLICE="$(curl -s "$URL/rows?path=orders.csv&count=1&start-col=1&count-cols=1")"
has "rows: horizontal slice"     "$SLICE" '"columns":["cust_no"]'

CELL="$(curl -s "$URL/cell?path=orders.csv&row=1&col=city")"
has "cell: value + key"          "$CELL" '"key":"b:0"'

echo "== writes and guards =="
CURRENT="$(curl -s "$URL/cell?path=orders.csv&row=1&col=city" | sed -n 's/.*"value":"\([^"]*\)".*/\1/p')"
want_code_post "409 on a wrong expect" 409 "/edit?path=orders.csv&action=set&row=1&col=city&value=X&expect=__no__"
WRITE="$(curl -s -X POST "$URL/edit?path=orders.csv&action=set&row=1&col=city&value=Z&expect=$CURRENT")"
if printf '%s' "$WRITE" | grep -qF '"op"'; then printf '  ok   write with the right expect (%s)\n' "$CURRENT"
else printf '  FAIL write with the right expect: %s\n' "$(printf '%s' "$WRITE" | cut -c1-120)"; failed=$((failed + 1)); fi
want_code_post "409 once the value moved" 409 "/edit?path=orders.csv&action=set&row=1&col=city&value=W&expect=$CURRENT"

JOURNAL="$(curl -s "$URL/journal?path=orders.csv")"
has "journal: count"             "$JOURNAL" '"count":1'
has "journal: description"       "$JOURNAL" "set row 1 'city'"
has "journal: canUndo"           "$JOURNAL" '"canUndo":true'

echo "== keys and summary =="
KEYS="$(curl -s "$URL/rows?path=orders.csv&keys=b:2,b:0")"
has "keys: row numbers"        "$KEYS" '"rowNumbers":[3,1]'
has "keys: rows"               "$KEYS" '"rows":['
KEYSW="$(curl -s "$URL/rows?path=orders.csv&keys=b:2&columns=id,city")"
has "keys: projection applies" "$KEYSW" '"columns":["id","city"]'
SHEET="$(curl -s "$URL/sheet?path=orders.csv")"
has "sheet: operations count"  "$SHEET" '"operations":'
has "sheet: lastOperation"     "$SHEET" '"lastOperation"'
has "sheet: capabilities"      "$SHEET" '"capabilities":'

echo "== async export =="
JOB="$(curl -s -X POST "$URL/edit?path=orders.csv&action=export&out=exported.csv&where=city=CDMX" | sed -n 's/.*"job":"\([^"]*\)".*/\1/p')"
if [ -n "$JOB" ]; then printf '  ok   export queued a job (%s)\n' "$JOB"
else printf '  FAIL export did not queue a job\n'; failed=$((failed + 1)); fi
for _ in $(seq 1 60); do
  STATE="$(curl -s "$URL/job?id=$JOB")"
  printf '%s' "$STATE" | grep -q '"done"' && break
  sleep 0.5
done
has "export job finished"  "$STATE" '"done"'
has "export job output"    "$STATE" '"output"'
if [ -s exported.csv ]; then printf '  ok   exported file written (%s bytes)\n' "$(wc -c < exported.csv | tr -d ' ')"
else printf '  FAIL exported file is missing or empty\n'; failed=$((failed + 1)); fi
has "exported file has a header" "$(head -1 exported.csv)" "id,"

echo "== write error shapes =="
want_code_post "400 on an unknown action" 400 "/edit?path=orders.csv&action=nope"
want_code "400 on a missing col"          400 "/rows?path=orders.csv&count=1&columns=nosuch"

echo "== error shapes =="
want_code "400 on unknown endpoint"  404 "/nope"
want_code "400 on bad column"        400 "/rows?path=orders.csv&order=nosuch"
want_code "400 on bad SQL"           400 "/query?path=orders.csv&sql=SELECT%20nope%20FROM%20t"
want_code "404 past the end"         404 "/cell?path=orders.csv&row=999999&col=city"
want_code "400 on bad path"          400 "/rows?path=../../../etc/passwd"

if [ "$failed" -ne 0 ]; then
  echo "== $failed check(s) failed =="
  exit 1
fi
echo "== backend surface answered =="
