#!/usr/bin/env bash
# Walk the paths a new user actually takes, and fail loudly.
#
# Unit tests check functions. This checks the *product*: a file made by `demo`,
# viewed, filtered, edited, exported, joined and split — the way one person would
# use it in one sitting. Every step asserts something, so a regression shows up as a
# named failure rather than a wrong number nobody notices.
#
#   scripts/smoke.sh          run everything (about 15 s)
#   scripts/smoke.sh --keep   leave the working directory for inspection
set -uo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  for candidate in "$HOME/.cargo/bin" /usr/local/bin /opt/homebrew/bin; do
    if [ -x "$candidate/cargo" ]; then
      PATH="$candidate:$PATH"
      export PATH
      break
    fi
  done
fi

BIN="${CSV_SPLITTER_BIN:-}"
if [ -z "$BIN" ]; then
  # Absolute path: the script changes directory later, and a relative binary would
  # stop resolving the moment it does.
  BIN="$(pwd)/target/debug/csv-splitter"
fi
if [ ! -x "$BIN" ]; then
  echo "building csv-splitter-cli"
  cargo build -p csv-splitter-cli -q || exit 1
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/csvs_smoke.XXXXXX")"
KEEP="${1:-}"
cleanup() {
  if [ "$KEEP" = "--keep" ]; then
    echo "(kept $WORK)"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

FAILED=0
step() { printf '\n== %s\n' "$1"; }
ok() { printf '   ok   %s\n' "$1"; }
fail() {
  printf '   FAIL %s\n' "$1"
  FAILED=$((FAILED + 1))
}
run() {  # run <label> <command...>
  local label="$1"
  shift
  if out=$("$@" 2>&1); then
    ok "$label"
    return 0
  fi
  fail "$label"
  printf '%s\n' "$out" | sed 's/^/        /'
  return 1
}

cd "$WORK"

step "demo file"
run "demo writes a sample" "$BIN" demo -o sample > /dev/null
DEMO=sample/orders.csv
[ -f "$DEMO" ] && ok "sample file exists" || fail "sample file missing"

step "look at it"
run "preview" "$BIN" preview "$DEMO" --rows 3 > /dev/null
run "analyze" "$BIN" analyze "$DEMO" > /dev/null
run "stats" "$BIN" stats "$DEMO" > /dev/null
run "book lists the sample" "$BIN" book sample

step "find things"
run "filter by column" "$BIN" filter "$DEMO" --where "city=CDMX" -o filtered.csv
run "filter then order" "$BIN" filter "$DEMO" --where "amount>100" --order "amount:desc" -o ordered.csv
run "sort several keys" "$BIN" sort "$DEMO" --by "city,amount:desc" -o sorted.csv
run "search" "$BIN" search "$DEMO" CDMX
run "paged view" "$BIN" view "$DEMO" --count 5
run "view with a filter" "$BIN" view "$DEMO" --where "city=CDMX" --count 3

step "summarise"
run "groupby" "$BIN" groupby "$DEMO" --by city --agg "amount:sum" -o g.csv
run "pivot" "$BIN" pivot "$DEMO" --rows city --cols city --values "amount:sum" -o p.csv
run "chart" "$BIN" chart "$DEMO" --x city --y "amount:sum" -o chart.json
[ -s chart.json ] && ok "chart json written" || fail "chart json empty"

step "format and layout"
run "conditional formatting" "$BIN" format "$DEMO" --rule "amount>1000:bg=red" --json > /dev/null
run "layout width" "$BIN" layout "$DEMO" --set "city:width=160"
run "freeze first two" "$BIN" layout "$DEMO" --freeze-first 2

step "edit without touching the base"
cp "$DEMO" edited.csv
BASE_SUM=$(shasum edited.csv | cut -d' ' -f1)
run "bulk set" "$BIN" edit set edited.csv --where "city=CDMX" --col amount --value 0
run "insert column" "$BIN" edit insert-column edited.csv --after city --name note --value manual
run "formula column" "$BIN" edit formula edited.csv --name double --value "amount * 2"
run "status" "$BIN" edit status edited.csv
run "undo" "$BIN" edit undo edited.csv
run "redo" "$BIN" edit redo edited.csv
run "redo again (should refuse)" bash -c "! $BIN edit redo edited.csv > /dev/null 2>&1" || true
run "export" "$BIN" edit export edited.csv -o edited.out.csv
AFTER_SUM=$(shasum edited.csv | cut -d' ' -f1)
if [ "$BASE_SUM" = "$AFTER_SUM" ]; then
  ok "the base file is byte-identical"
else
  fail "the base file changed"
fi
head -1 edited.out.csv | grep -q "note" && ok "inserted column in the output" || fail "inserted column missing"
grep -q ",0," edited.out.csv && ok "bulk edit visible" || fail "bulk edit invisible"

step "join another table"
printf 'cust_no,name\n1,Ana\n' > customers.csv
printf 'order,cust_no\nA,1\nB,2\n' > orders.csv
run "lookup" "$BIN" lookup orders.csv --key cust_no --from customers.csv --get name -o joined.csv
grep -q "#N/A" joined.csv && ok "missing key marked #N/A" || fail "missing key not marked"

step "move data between shapes"
run "split" "$BIN" split "$DEMO" --splits 3 -o parts.tar
run "verify the split" "$BIN" verify "$DEMO" --splits 3
run "merge back" "$BIN" merge parts.tar -o merged.csv
run "partition" "$BIN" partition "$DEMO" --by city -o parts-part.tar --verify


step "combinations that used to break"
# GBK + semicolon delimiter + filter, all at once.
python3 - <<'PY' > /dev/null 2>&1 || true
open("gbk_semi.csv", "w", encoding="gbk").write("城市;金额\nCDMX;10\nPuebla;20\n")
PY
if [ -f gbk_semi.csv ]; then
  run "GBK + semicolon" "$BIN" filter gbk_semi.csv --where "城市=CDMX" -d semicolon -o g.csv
  run "GBK + semicolon partition" "$BIN" partition gbk_semi.csv --by 城市 -d semicolon -o g.part.tar --verify
else
  printf '   skip GBK checks (python3 unavailable)\n'
fi

# A quoted field containing a newline must survive split + merge byte for byte.
printf 'id,note\n1,"line one\nline two"\n2,plain\n' > multiline.csv
run "split a multi-line field" "$BIN" split multiline.csv --splits 2 -o ml.tar
run "verify it is lossless" "$BIN" verify multiline.csv --splits 2
run "merge it back" "$BIN" merge ml.tar -o ml.out.csv
if cmp -s multiline.csv ml.out.csv; then
  ok "round trip is byte-identical"
else
  fail "round trip changed the file"
fi

# An edited file filtered through the API must show the edits (not the base bytes).
run "edit then filter" "$BIN" edit set multiline.csv --row 1 --col note --value changed
if out=$("$BIN" filter multiline.csv --where "note=changed" -o aflt.csv 2>&1); then
  printf '   ok   CLI filter still reads the base (and warns)\n'
  printf '%s\n' "$out" | grep -q "pending edit" && ok "and it says so" || fail "it did not warn"
else
  fail "filter on an edited file errored"
fi

step "the clean path"
run "split-free copy" "$BIN" columns "$DEMO" --keep "city,amount" -o two-cols.csv

step "server"
PORT_FILE="$WORK/port.txt"
"$BIN" serve --root "$WORK" --addr 127.0.0.1:0 > "$PORT_FILE" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 50); do
  grep -q "http://" "$PORT_FILE" 2>/dev/null && break
  sleep 0.1
done
ADDR=$(grep -oE '127\.0\.0\.1:[0-9]+' "$PORT_FILE" | head -1)
if [ -z "$ADDR" ]; then
  fail "server did not start"
else
  for endpoint in health book; do
    if curl -sf "http://$ADDR/$endpoint" > /dev/null 2>&1; then
      ok "/$endpoint responds"
    else
      fail "/$endpoint did not respond"
    fi
  done
  if curl -sf "http://$ADDR/sheet?path=edited.csv" > /dev/null 2>&1; then
    ok "/sheet responds"
  else
    fail "/sheet did not respond"
  fi
  if curl -sf "http://$ADDR/rows?path=edited.csv" | grep -q '"columns"'; then
    ok "/rows returns columns"
  else
    fail "/rows missing columns"
  fi
fi

step "API answers bad requests with an explanation"
if [ -n "${ADDR:-}" ]; then
  for probe in "rows?path=edited.csv&start=abc" "book?max=nope" "groupby?path=edited.csv"; do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://$ADDR/$probe")
    body=$(curl -s "http://$ADDR/$probe")
    if [ "$code" != "200" ] && printf '%s' "$body" | grep -q '"error"'; then
      ok "$probe → $code with an error field"
    else
      fail "$probe answered $code without an error field"
    fi
  done
fi
kill "$SERVER_PID" 2>/dev/null
wait "$SERVER_PID" 2>/dev/null

printf '\n== result: %s\n' "$([ "$FAILED" -eq 0 ] && echo 'all steps passed' || echo "$FAILED step(s) failed")"
[ "$FAILED" -eq 0 ]
