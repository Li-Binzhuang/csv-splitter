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

# The deployment self-check, before the server is even started — the same command a
# systemd ExecStartPre runs.
CHECK_OUT="$("$BIN" serve --check --root "$WORK" --addr 127.0.0.1:0 2>&1)"
has "deploy self-check: root readable" "$CHECK_OUT" 'root readable'
has "deploy self-check: bind"          "$CHECK_OUT" 'bind'
has "deploy self-check: ok"            "$CHECK_OUT" 'configuration ok'

echo "== framework surface =="
CAPS="$(curl -s "$URL/capabilities")"
has "capabilities list reads"    "$CAPS" '"/rows"'
has "capabilities list writes"   "$CAPS" '"POST /edit"'
has "capabilities list actions"  "$CAPS" '"set"'
has "capabilities: strict"       "$CAPS" '"strictParams":true'
has "capabilities: duckdb"       "$CAPS" '"duckdb":'
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

echo "== browser surface =="
SPEC="$(curl -s "$URL/spec")"
has "spec: version"            "$SPEC" '"version":'
has "spec: fields for /rows"   "$SPEC" '"fields":{"/rows":['
has "spec: error codes"        "$SPEC" '"504":'
PREFLIGHT="$(curl -s -o /dev/null -w '%{http_code}' -X OPTIONS "$URL/rows?path=orders.csv" -H 'Origin: http://localhost:3000')"
if [ "$PREFLIGHT" = "204" ]; then printf '  ok   preflight answers 204\n'
else printf '  FAIL preflight returned %s\n' "$PREFLIGHT"; failed=$((failed + 1)); fi
CORS="$(curl -s -D - -o /dev/null "$URL/rows?path=orders.csv&count=1")"
has "cors: allow methods"     "$CORS" 'Access-Control-Allow-Methods: GET, POST, HEAD, OPTIONS'
has "etag: on a window"       "$CORS" 'ETag:'
HEAD_CODE="$(curl -s -o /dev/null -w '%{http_code}' -I "$URL/rows?path=orders.csv&count=1")"
if [ "$HEAD_CODE" = "200" ]; then printf '  ok   HEAD answers 200\n'
else printf '  FAIL HEAD returned %s\n' "$HEAD_CODE"; failed=$((failed + 1)); fi

echo "== compression =="
GZIP_HEAD="$(curl -s -D - -o /dev/null -H 'Accept-Encoding: gzip' "$URL/rows?path=orders.csv&count=200")"
has "gzip: encoding header"    "$GZIP_HEAD" 'Content-Encoding: gzip'
has "gzip: vary header"        "$GZIP_HEAD" 'Vary: Accept-Encoding'
PLAIN_LEN="$(curl -s "$URL/rows?path=orders.csv&count=200" | wc -c | tr -d ' ')"
GZIP_LEN="$(curl -s -H 'Accept-Encoding: gzip' "$URL/rows?path=orders.csv&count=200" | wc -c | tr -d ' ')"
if [ "$GZIP_LEN" -lt "$PLAIN_LEN" ]; then printf '  ok   gzip: smaller body (%s -> %s bytes)\n' "$PLAIN_LEN" "$GZIP_LEN"
else printf '  FAIL gzip did not shrink the body (%s -> %s)\n' "$PLAIN_LEN" "$GZIP_LEN"; failed=$((failed + 1)); fi

echo "== robustness =="
# A client that asks and walks away must not take the server with it.
( curl -s -m 1 "$URL/rows?path=orders.csv&count=2000" >/dev/null 2>&1 & ) ; sleep 0.2
HEALTH_AFTER="$(curl -s "$URL/health")"
has "survives a hangup"        "$HEALTH_AFTER" '"ok":true'
# Twenty at once, alternating a filtered page and a metrics read.
# PIDs are collected and waited on individually: a bare `wait` would also wait for the
# server this script started, which never exits.
CONC_PIDS=""
for i in $(seq 1 20); do
  if [ $((i % 2)) -eq 0 ]; then
    curl -s -o /dev/null -w '%{http_code}' "$URL/rows?path=orders.csv&where=city%3DCDMX&count=5&columns=id,city" > "$WORK/conc_$i.txt" &
  else
    curl -s -o /dev/null -w '%{http_code}' "$URL/metrics" > "$WORK/conc_$i.txt" &
  fi
  CONC_PIDS="$CONC_PIDS $!"
done
for pid in $CONC_PIDS; do wait "$pid" 2>/dev/null; done
CONC_FAILED=0
for i in $(seq 1 20); do
  CODE="$(cat "$WORK/conc_$i.txt" 2>/dev/null)"
  if [ "$CODE" != "200" ]; then CONC_FAILED=$((CONC_FAILED + 1)); fi
done
if [ "$CONC_FAILED" = "0" ]; then printf '  ok   20 concurrent requests all 200\n'
else printf '  FAIL %s of 20 concurrent requests failed\n' "$CONC_FAILED"; failed=$((failed + 1)); fi

echo "== parity with the CLI =="
VALIDATE_JSON="$(curl -s "$URL/validate?path=orders.csv&checks=amount%3E%3D0;city:not-empty")"
has "validate: report"        "$VALIDATE_JSON" '"rows":'
has "validate: per-rule counts" "$VALIDATE_JSON" '"counts":'
LIMITED_JSON="$(curl -s "$URL/validate?path=orders.csv&checks=amount%3E%3D0&limit=100")"
has "validate: limited scan is partial" "$LIMITED_JSON" '"partial":true'
has "validate: limited scan checked 100" "$LIMITED_JSON" '"checked":100'
printf 'city,sales\nCDMX,10\nPuebla,20\n' > "$WORK/regions.csv"
LOOKUP_JOB="$(curl -s -X POST "$URL/lookup?path=orders.csv&key=city&from=regions.csv&get=sales&out=orders-lookup.csv" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("job",""))')"
has "lookup: job accepted" "$LOOKUP_JOB" 'job'
for _ in $(seq 1 40); do
  LOOKUP_STATE="$(curl -s "$URL/job?id=$LOOKUP_JOB")"
  echo "$LOOKUP_STATE" | grep -q '"state":"done"' && break
  sleep 0.25
done
has "lookup: finished"  "$LOOKUP_STATE" '"state":"done"'
if head -1 "$WORK/orders-lookup.csv" | grep -q sales; then printf '  ok   lookup: wrote the column\n'
else printf '  FAIL lookup wrote no column\n'; failed=$((failed + 1)); fi

echo "== schema diff and concat =="
printf 'id,city,qty\n1,Puebla,5\n' > "$WORK/shape-a.csv"
printf 'id,qty,city,extra\n1,5,Puebla,x\n' > "$WORK/shape-b.csv"
DIFF_JSON="$(curl -s "$URL/schema?path=shape-a.csv&diff=shape-b.csv")"
has "schema diff: added column" "$DIFF_JSON" '"added":["extra"]'
has "schema diff: moved column" "$DIFF_JSON" '"moved":'
# The same header as orders.csv: concat stacks files that agree on columns, and
# deliberately refuses a mismatch instead of producing a ragged file.
printf 'id,cust_no,city,amount,note,ts\n900001,C000001,CDMX,10.00,stacked row,2026-01-01\n' > "$WORK/extra-rows.csv"
CONCAT_JOB="$(curl -s -X POST "$URL/concat?paths=orders.csv,extra-rows.csv&out=stacked.csv" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("job",""))')"
for _ in $(seq 1 40); do
  CONCAT_STATE="$(curl -s "$URL/job?id=$CONCAT_JOB")"
  echo "$CONCAT_STATE" | grep -q '"state":"done"' && break
  sleep 0.25
done
has "concat: finished" "$CONCAT_STATE" '"state":"done"'
ORDERS_LINES="$(wc -l < orders.csv | tr -d ' ')"
STACKED_LINES="$(wc -l < "$WORK/stacked.csv" 2>/dev/null | tr -d ' ')"
if [ "${STACKED_LINES:-0}" -gt "$ORDERS_LINES" ]; then printf '  ok   concat: stacked %s lines over %s\n' "$STACKED_LINES" "$ORDERS_LINES"
else printf '  FAIL concat wrote %s lines, orders.csv has %s\n' "${STACKED_LINES:-0}" "$ORDERS_LINES"; failed=$((failed + 1)); fi

echo "== reshape and formula =="
COLUMNS_JOB="$(curl -s -X POST "$URL/columns?path=orders.csv&out=reduced.csv&keep=id,city&rename=city:town" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("job",""))')"
for _ in $(seq 1 40); do
  COLUMNS_STATE="$(curl -s "$URL/job?id=$COLUMNS_JOB")"
  echo "$COLUMNS_STATE" | grep -q '"state":"done"' && break
  sleep 0.25
done
has "columns reshape: finished" "$COLUMNS_STATE" '"state":"done"'
has "columns reshape: new header" "$(head -1 "$WORK/reduced.csv" 2>/dev/null)" "id,town"
FORMULA_JSON="$(curl -s -X POST "$URL/edit?path=orders.csv&action=formula&name=double&value=amount*2")"
has "edit formula: accepted"  "$FORMULA_JSON" '"op":"addcolumn"'
has "edit formula: recorded"  "$FORMULA_JSON" '"operations":'
ROWS_JSON="$(curl -s "$URL/rows?path=orders.csv&count=2&columns=id,double")"
has "edit formula: computed"  "$ROWS_JSON" '"double"'
# One undo removes the whole computed column. The fixture's journal may already hold
# other operations, so the count is not asserted here — the unit test owns that on a
# fresh file.
curl -s -X POST "$URL/edit?path=orders.csv&action=undo" > /dev/null
AFTER_UNDO="$(curl -s "$URL/columns?path=orders.csv")"
if echo "$AFTER_UNDO" | grep -q '"name":"double"'; then
  printf '  FAIL undo left the computed column\n'; failed=$((failed + 1))
else
  printf '  ok   edit formula: one undo removes it\n'
fi

echo "== column recompute =="
# A fresh fixture with known columns: reusing a file from an earlier step made the
# first assertion fail on a column that does not exist there.
printf 'id,qty,price\n1,2,5\n2,3,7\n' > "$WORK/recompute.csv"
rm -f "$WORK/recompute.csv.journal"
SETF="$(curl -s -X POST "$URL/edit?path=recompute.csv&action=set-formula&col=qty&value=price*10")"
has "set-formula: accepted" "$SETF" '"op":"setformula"'
has "set-formula: described" "$SETF" 'compute'
CYCLE="$(curl -s -X POST "$URL/edit?path=recompute.csv&action=set-formula&col=qty&value=qty*2")"
# Reading your own column is in-place recompute, not recursion: the row is built from the
# base first. A *new* column reading itself is what must be refused.
has "set-formula: self-recompute accepted" "$CYCLE" '"op":"setformula"'
NEWC="$(curl -s -X POST "$URL/edit?path=recompute.csv&action=formula&value=total*2&name=total")"
has "new column: self-reference refused" "$NEWC" 'reads its own column'
COMPUTED="$(curl -s "$URL/cell?path=recompute.csv&row=1&col=qty")"
has "set-formula: value computed" "$COMPUTED" '"value":"50"'

echo "== insert by key =="
printf 'id,tag\n1,a\n2,b\n3,c\n' > "$WORK/keys.csv"
rm -f "$WORK/keys.csv.journal"
curl -s -X POST "$URL/edit?path=keys.csv&action=insert-after-key&key=b:1&value=2.5,added" > /dev/null
KEY_ROWS="$(curl -s "$URL/rows?path=keys.csv&count=6&columns=id,tag")"
has "insert by key: placed after b:1" "$KEY_ROWS" '["2","b"],["2.5","added"],["3","c"]'
MISSING_KEY="$(curl -s -X POST "$URL/edit?path=keys.csv&action=insert-after-key&key=b:99&value=x,y")"
has "insert by key: missing key 404" "$MISSING_KEY" '"parameter":"key"'

echo "== multi-column fill =="
printf 'id,a,b\n1,x,y\n2,x,y\n3,x,y\n' > "$WORK/fill.csv"
rm -f "$WORK/fill.csv.journal"
FILL_JSON="$(curl -s -X POST "$URL/edit?path=fill.csv&action=fill&cols=a,b&from-row=1&to-row=2&value=0")"
has "fill cols: one operation" "$FILL_JSON" '"op":"fillcols"'
FILL_ROWS="$(curl -s "$URL/rows?path=fill.csv&count=3&columns=id,a,b")"
has "fill cols: both columns" "$FILL_ROWS" '["1","0","0"]'
curl -s -X POST "$URL/edit?path=fill.csv&action=undo" > /dev/null
UNDONE="$(curl -s "$URL/rows?path=fill.csv&count=3&columns=id,a,b")"
has "fill cols: one undo" "$UNDONE" '["1","x","y"]'

echo "== encoding convert =="
printf 'id,city\n1,Puebla\n' > "$WORK/utf8-src.csv"
CONVERT_JOB="$(curl -s -X POST "$URL/convert?path=utf8-src.csv&out=utf8-src.bom.csv&to=utf-8-bom" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("job",""))')"
for _ in $(seq 1 40); do
  CONVERT_STATE="$(curl -s "$URL/job?id=$CONVERT_JOB")"
  echo "$CONVERT_STATE" | grep -q '"state":"done"' && break
  sleep 0.25
done
has "convert: finished" "$CONVERT_STATE" '"state":"done"'
if [ -s "$WORK/utf8-src.bom.csv" ]; then printf '  ok   convert: wrote the file\n'
else printf '  FAIL convert wrote nothing\n'; failed=$((failed + 1)); fi
BAD_ENC="$(curl -s -X POST "$URL/convert?path=utf8-src.csv&out=x.csv&from=klingon")"
has "convert: unknown encoding 400" "$BAD_ENC" '"parameter":"from"'

echo "== cost tier =="
SHEET_COST="$(curl -s "$URL/sheet?path=orders.csv")"
has "sheet: cost tier"         "$SHEET_COST" '"tier":'
has "sheet: first page estimate" "$SHEET_COST" '"filterFirstPageMs":'
cp orders.csv fresh.csv
COLUMNS_JSON="$(curl -s "$URL/columns?path=fresh.csv&sample=5")"
if [ -f fresh.csv.idx ]; then printf '  FAIL columns built an index sidecar\n'; failed=$((failed + 1));
else printf '  ok   columns: no index sidecar\n'; fi
has "columns: rows unknown without index" "$COLUMNS_JSON" '"rows":null'
rm -f fresh.csv

echo "== limits =="
LONG="$(python3 -c "print('x'*17000)")"
CODE="$(curl -s -o /dev/null -w '%{http_code}' "$URL/rows?path=orders.csv&pad=$LONG")"
if [ "$CODE" = "414" ]; then printf '  ok   limits: long query -> 414\n'
else printf '  FAIL long query returned %s\n' "$CODE"; failed=$((failed + 1)); fi

echo "== job control =="
CANCEL_UNKNOWN="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$URL/job?action=cancel&id=nope")"
if [ "$CANCEL_UNKNOWN" = "404" ]; then printf '  ok   cancel: unknown job -> 404\n'
else printf '  FAIL cancel unknown returned %s\n' "$CANCEL_UNKNOWN"; failed=$((failed + 1)); fi

EXPORT_RESP="$(curl -s -X POST "$URL/edit?path=orders.csv&action=export&out=cancel.csv&where=city=CDMX")"
JOB_ID="$(printf '%s' "$EXPORT_RESP" | sed -n 's/.*"job":"\([^"]*\)".*/\1/p')"
if [ -n "$JOB_ID" ]; then
  JOB_JSON="$(curl -s "$URL/job?id=$JOB_ID")"
  has "jobs: cancellable flag" "$JOB_JSON" '"cancellable":true'
else printf '  FAIL export did not queue a job\n'; failed=$((failed + 1)); fi
rm -f cancel.csv

echo "== observability =="
HEALTH="$(curl -s "$URL/health")"
has "health: ok"               "$HEALTH" '"ok":true'
has "health: ready"            "$HEALTH" '"ready":true'
has "health: version"          "$HEALTH" '"version":'
METRICS="$(curl -s "$URL/metrics")"
has "metrics: requests"        "$METRICS" '"requests":'
has "metrics: percentiles"     "$METRICS" '"p95Ms":'
has "metrics: routes"          "$METRICS" '"GET /rows"'
TIMING="$(curl -s -D - -o /dev/null "$URL/rows?path=orders.csv&count=1")"
has "timing header"            "$TIMING" 'X-Response-Time:'

echo "== columnar cache =="
has "cache: advertised"        "$CAPS" '"caches":["parquet"]'
has "cache: listed as write"   "$CAPS" 'POST /parquet'
PARQ="$(curl -s -X POST "$URL/parquet?path=orders.csv")"
if printf '%s' "$PARQ" | grep -qE '"job"|"cached"'; then printf '  ok   cache: job or cached\n'
else printf '  FAIL cache: neither job nor cached\n'; failed=$((failed + 1)); fi
CACHE_STATE="$(curl -s "$URL/sheet?path=orders.csv")"
has "cache: sheet reports it"  "$CACHE_STATE" '"parquet":{'
if printf '%s' "$PARQ" | grep -q '"job":"'; then
  JOB="$(printf '%s' "$PARQ" | sed -n 's/.*"job":"\([^"]*\)".*/\1/p')"
  for _ in $(seq 1 120); do
    STATE="$(curl -s "$URL/job?id=$JOB")"
    printf '%s' "$STATE" | grep -q '"done"' && break
    sleep 0.5
  done
  has "cache: build finished" "$STATE" '"done"'
fi
WARMED="$(curl -s "$URL/rows?path=orders.csv&where=city%3DPuebla&count=5&columns=id,city")"
has "cache: filtered page still right" "$WARMED" '"rows":'

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
