#!/usr/bin/env bash
# End-to-end product demo: pipeline + MCP client. No GUI, no network, no xlsx.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${BIN:-target/release/csvx}
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
cat > "$DIR/plan.csv" <<'PLAN'
step,input,command,args,output
filtered,orders.csv,filter,--where city=CDMX -o {out},filtered.csv
quality,filtered,analyze,--quality --json --key id --group city,quality.json
PLAN
echo "== pipeline =="
"$BIN" pipeline run "$DIR/plan.csv" --root "$DIR"
echo "== MCP client =="
python3 scripts/mcp_client.py --binary "$BIN" --root "$DIR" --file orders.csv
echo "== accepted =="
