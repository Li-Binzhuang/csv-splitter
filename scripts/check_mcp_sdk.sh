#!/usr/bin/env bash
# Official MCP SDK end-to-end check. Skips when node or the SDK install is absent.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v node >/dev/null 2>&1 || [[ ! -d scripts/mcp_sdk/node_modules ]]; then
  echo "mcp_sdk=skip (node or scripts/mcp_sdk/node_modules missing)"
  exit 0
fi

if [[ -n "${CSVX_BIN:-}" ]]; then
  binary=$CSVX_BIN
else
  binary=target/release/csvx
  [[ -x "$binary" ]] || binary=target/debug/csvx
fi
[[ -x "$binary" ]] || { echo "mcp_sdk=skip (binary missing)"; exit 0; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf 'id,city,amount\n1,CDMX,10\n2,Puebla,20\n' > "$tmp/orders.csv"

out=$(node scripts/mcp_sdk/client.mjs --binary "$binary" --root "$tmp" --file orders.csv)
echo "$out"

grep -q '^sdk_tools=23$' <<<"$out" || { echo "mcp_sdk=fail tools"; exit 1; }
grep -q '^sdk_annotated=23$' <<<"$out" || { echo "mcp_sdk=fail annotations"; exit 1; }
grep -q '^progress_events=[1-9]' <<<"$out" || { echo "mcp_sdk=fail progress"; exit 1; }
grep -q '^structured_ok=true$' <<<"$out" || { echo "mcp_sdk=fail structured"; exit 1; }
grep -q '^job=done$' <<<"$out" || { echo "mcp_sdk=fail job"; exit 1; }
echo "mcp_sdk=ok"
