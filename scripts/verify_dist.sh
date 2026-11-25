#!/usr/bin/env bash
# Verify an offline release tarball: checksum, members, and the binary.
set -euo pipefail
cd "$(dirname "$0")/.."
TAR=${1:-}
if [[ -z "$TAR" || ! -f "$TAR" ]]; then
  echo "usage: bash scripts/verify_dist.sh <dist.tar.gz>" >&2
  exit 2
fi
if [[ ! -f "$TAR.sha256" ]]; then
  echo "missing checksum: $TAR.sha256" >&2
  exit 2
fi
shasum -a 256 -c "$TAR.sha256"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
tar -xzf "$TAR" -C "$STAGE"
ROOT=$(find "$STAGE" -maxdepth 1 -mindepth 1 -type d | head -1)
for required in csvx README.md AGENT_QUICKSTART.md TRY_CSVX.sh docs/RELEASE_CHECKLIST.md docs/ROADMAP.md docs/NIGHT_CHECKPOINT.md mcp_sdk/client.mjs examples/agent/http_session.sh examples/try_csvx.sh scripts/agent_routes_smoke.py; do
  if [[ ! -f "$ROOT/$required" ]]; then
    echo "missing member: $required" >&2
    exit 1
  fi
done
"$ROOT/csvx" --version
CSVX_BIN="$ROOT/csvx" bash "$ROOT/TRY_CSVX.sh" >/dev/null

AGENT_DIR=$(mktemp -d)
printf 'id,city,amount\n1,CDMX,10\n2,Puebla,20\n' > "$AGENT_DIR/orders.csv"
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
"$ROOT/csvx" agent serve --root "$AGENT_DIR" --addr "127.0.0.1:$PORT" >"$AGENT_DIR/server.log" 2>&1 &
AGENT_PID=$!
trap 'kill "$AGENT_PID" 2>/dev/null || true; rm -rf "$STAGE" "$AGENT_DIR"' EXIT
for _ in $(seq 1 50); do
  if python3 -c "import urllib.request,sys; urllib.request.urlopen('http://127.0.0.1:$PORT/agent/v1/manifest', timeout=1)" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
AGENT_OUT=$(python3 "$ROOT/scripts/agent_routes_smoke.py" --api "http://127.0.0.1:$PORT" --file orders.csv --binary "$ROOT/csvx" --root "$AGENT_DIR")
printf '%s\n' "$AGENT_OUT"
printf 'stats: %s\n' "$AGENT_OUT"
kill "$AGENT_PID" 2>/dev/null || true
wait "$AGENT_PID" 2>/dev/null || true

echo "verified: $TAR"
