#!/usr/bin/env bash
# Optional real-DSH MCP check. It boots a model run, so it is opt-in only.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "${CSVX_DSH_TEST:-0}" != "1" ]]; then
  echo "mcp_dsh=skip (set CSVX_DSH_TEST=1 to run a real DSH headless task)"
  exit 0
fi
if ! command -v dsh >/dev/null 2>&1; then
  echo "mcp_dsh=skip (dsh not on PATH)" >&2
  exit 0
fi

if [[ -n "${CSVX_BIN:-}" ]]; then
  binary=$CSVX_BIN
else
  binary=target/debug/csvx
  [[ -x "$binary" ]] || binary=target/release/csvx
fi
[[ -x "$binary" ]] || { echo "mcp_dsh=skip (csvx binary missing)"; exit 0; }
binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")

root=$(mktemp -d)
patch=$(mktemp)
trap 'rm -rf "$root" "$patch"' EXIT
printf 'id,city\n1,CDMX\n2,Puebla\n' > "$root/orders.csv"
cat > "$patch" <<YML
- insert:
    - id: csvx-mcp-dsh-check
      name: "@deepseek-ai/dsh-mcp-client"
      config:
        serverName: csvx
        transport: stdio
        command: "$binary"
        args: ["mcp", "--root", "$root"]
        failOnStartupError: true
YML

out=$(dsh headless --patch "$patch" --permission-mode read-only \
  "Use the csvx MCP server to call its csvx_schema tool on $root/orders.csv and report the row count. Do not edit any file." 2>&1)
echo "$out" | tail -n 12

grep -qiE '2 rows|row count.*2|rows.*2' <<<"$out" || { echo "mcp_dsh=fail rows" >&2; exit 1; }
grep -qi 'city' <<<"$out" || { echo "mcp_dsh=fail columns" >&2; exit 1; }
echo "mcp_dsh=ok"
