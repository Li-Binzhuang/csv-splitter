#!/usr/bin/env bash
# End-to-end product demo: pipeline + MCP client. No GUI, no network, no xlsx.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${BIN:-target/release/csvx}
if [[ ! -x "$BIN" ]]; then
  echo "error: $BIN is missing; run cargo build --release (or set CSVX_DEMO_BUILD=1)" >&2
  exit 1
fi
if find crates -name '*.rs' -newer "$BIN" -print -quit | grep -q .; then
  if [[ "${CSVX_DEMO_BUILD:-0}" == "1" ]]; then
    cargo build --release -p csvx-cli
  else
    echo "error: $BIN is older than Rust sources; run cargo build --release (or set CSVX_DEMO_BUILD=1)" >&2
    exit 1
  fi
fi
echo "== csvx version =="
"$BIN" --version
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT
cat > "$DIR/orders.csv" <<'CSV'
id,city,amount,customer
1,CDMX,10,C1
2,Puebla,20,C2
3,CDMX,30,C3
4,Tijuana,40,C4
5,CDMX,50,C5
CSV
if command -v gzip >/dev/null 2>&1; then
  gzip -c "$DIR/orders.csv" > "$DIR/orders.csv.gz"
fi
cat > "$DIR/plan.csv" <<'PLAN'
step,input,command,args,output
filtered,orders.csv,filter,--where city=CDMX -o {out},filtered.csv
quality,filtered,analyze,--quality --json --key id --group city,quality.json
PLAN
echo "== pipeline =="
"$BIN" pipeline run "$DIR/plan.csv" --root "$DIR"
echo "== MCP client =="
python3 scripts/mcp_client.py --binary "$BIN" --root "$DIR" --file orders.csv
if command -v node >/dev/null 2>&1 && [[ -d scripts/mcp_sdk/node_modules ]]; then
  echo "== MCP official SDK =="
  node scripts/mcp_sdk/client.mjs --binary "$BIN" --root "$DIR" --file orders.csv
else
  echo "(official SDK check skipped: run npm install in scripts/mcp_sdk first)"
fi
echo "== accepted =="
