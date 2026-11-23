#!/usr/bin/env bash
# Build a self-contained offline release tarball. No tag, no registry, no network.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${BIN:-target/release/csvx}
if [[ ! -x "$BIN" ]]; then
  cargo build --release -p csvx-cli
fi
VERSION=$("$BIN" --version | awk '{print $2}')
PLATFORM=$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)
NAME="csvx-$VERSION-$PLATFORM"
STAGE="dist/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/docs"
cp "$BIN" "$STAGE/csvx"
cp README.md CHANGELOG.md LICENSE "$STAGE/"
for doc in CAPABILITIES HANDOVER BENCHMARKS BUSINESS DEMO PILOT PILOT_SAMPLE PILOT_CONFIRMATION SALES OUTREACH RELEASE_CHECKLIST ROADMAP PILOT_INTAKE; do
  [[ -f "docs/$doc.md" ]] && cp "docs/$doc.md" "$STAGE/docs/"
done
cp scripts/demo_end_to_end.sh scripts/mcp_client.py scripts/pilot_report.py scripts/bench_mcp_cache.py "$STAGE/"
mkdir -p "$STAGE/mcp_sdk"
cp scripts/mcp_sdk/client.mjs scripts/mcp_sdk/package.json "$STAGE/mcp_sdk/"
tar -czf "dist/$NAME.tar.gz" -C dist "$NAME"
shasum -a 256 "dist/$NAME.tar.gz" > "dist/$NAME.tar.gz.sha256"
bash scripts/verify_dist.sh "dist/$NAME.tar.gz"
du -h "dist/$NAME.tar.gz"
cat "dist/$NAME.tar.gz.sha256"
