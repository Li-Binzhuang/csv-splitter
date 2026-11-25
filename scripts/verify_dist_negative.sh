#!/usr/bin/env bash
# Prove the release verifier rejects an incomplete package.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${BIN:-target/release/csvx}
if [[ ! -x "$BIN" ]]; then
  echo "error: missing $BIN" >&2
  exit 2
fi
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
PKG="$WORK/csvx-0.0.0-test"
mkdir -p "$PKG"
cp "$BIN" "$PKG/csvx"
cp README.md "$PKG/README.md"
# Deliberately omit AGENT_QUICKSTART, TRY_CSVX, docs and examples.
tar -czf "$WORK/bad.tar.gz" -C "$WORK" csvx-0.0.0-test
shasum -a 256 "$WORK/bad.tar.gz" > "$WORK/bad.tar.gz.sha256"
if bash scripts/verify_dist.sh "$WORK/bad.tar.gz" >"$WORK/log" 2>&1; then
  echo "error: incomplete package was accepted" >&2
  cat "$WORK/log" >&2
  exit 1
fi
grep -q 'missing member' "$WORK/log" || {
  echo "error: rejection reason missing" >&2
  cat "$WORK/log" >&2
  exit 1
}
test -f "$WORK/bad.verify.json"
grep -q '"verified":false' "$WORK/bad.verify.json"
echo "negative verification=ok"
