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
  binary=target/debug/csvx
  [[ -x "$binary" ]] || binary=target/release/csvx
fi
[[ -x "$binary" ]] || { echo "mcp_sdk=skip (binary missing)"; exit 0; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf 'id,city,amount\n1,CDMX,10\n2,Puebla,20\n' > "$tmp/orders.csv"
printf 'step,input,command,args,output\nfiltered,orders.csv,filter,--where city=CDMX -o {out},filtered.csv\n' > "$tmp/plan.csv"
gzip -c "$tmp/orders.csv" > "$tmp/orders.csv.gz"

out=$(node scripts/mcp_sdk/client.mjs --binary "$binary" --root "$tmp" --file orders.csv)
echo "$out"

grep -q '^sdk_tools=23$' <<<"$out" || { echo "mcp_sdk=fail tools"; exit 1; }
grep -q '^sdk_names=23$' <<<"$out" || { echo "mcp_sdk=fail names"; exit 1; }
grep -q '^sdk_annotated=23$' <<<"$out" || { echo "mcp_sdk=fail annotations"; exit 1; }
grep -q '^progress_events=[1-9]' <<<"$out" || { echo "mcp_sdk=fail progress"; exit 1; }
grep -q '^structured_ok=true$' <<<"$out" || { echo "mcp_sdk=fail structured"; exit 1; }
grep -q '^job=done$' <<<"$out" || { echo "mcp_sdk=fail job"; exit 1; }
grep -q '^pipeline_progress_events=[1-9]' <<<"$out" || { echo "mcp_sdk=fail pipeline progress"; exit 1; }
grep -q '^status_fresh=[1-9]' <<<"$out" || { echo "mcp_sdk=fail pipeline status"; exit 1; }
grep -q '^gzip_matched=1$' <<<"$out" || { echo "mcp_sdk=fail gzip"; exit 1; }
grep -q '^cache_hits=[1-9]' <<<"$out" || { echo "mcp_sdk=fail cache hits"; exit 1; }
grep -q '^cache_entries=[1-9]' <<<"$out" || { echo "mcp_sdk=fail cache entries"; exit 1; }
grep -q '^cache_after=0$' <<<"$out" || { echo "mcp_sdk=fail cache clear"; exit 1; }
grep -q '^cancel_state=done$' <<<"$out" || { echo "mcp_sdk=fail cancel"; exit 1; }
grep -q '^job_listed=true$' <<<"$out" || { echo "mcp_sdk=fail job list"; exit 1; }
grep -q '^job_reject_cache=true$' <<<"$out" || { echo "mcp_sdk=fail cache reject"; exit 1; }
grep -q '^job_reject_code=job_start_error$' <<<"$out" || { echo "mcp_sdk=fail reject code"; exit 1; }
grep -q '^unknown_tool_error=true$' <<<"$out" || { echo "mcp_sdk=fail unknown tool"; exit 1; }
grep -q '^unknown_code=unknown_tool$' <<<"$out" || { echo "mcp_sdk=fail unknown code"; exit 1; }
grep -q '^sdk_calls=23$' <<<"$out" || { echo "mcp_sdk=fail tool coverage"; exit 1; }
grep -q '^sdk_missing=$' <<<"$out" || { echo "mcp_sdk=fail missing tools"; exit 1; }
grep -q '^required_rejections=20$' <<<"$out" || { echo "mcp_sdk=fail required args"; exit 1; }
grep -q '^enum_rejections=2$' <<<"$out" || { echo "mcp_sdk=fail enum args"; exit 1; }
grep -q '^range_rejections=2$' <<<"$out" || { echo "mcp_sdk=fail range args"; exit 1; }
echo "mcp_sdk=ok"
