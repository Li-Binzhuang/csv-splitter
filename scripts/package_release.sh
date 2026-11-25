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
for doc in ACCEPTANCE AGENT_API AGENT_TRACE QUICKSTART_AGENT CAPABILITIES FORMULA_CACHE DSH_SKILL HANDOVER NIGHT_CHECKPOINT BENCHMARKS BUSINESS DEMO PILOT PILOT_SAMPLE PILOT_CONFIRMATION SALES OUTREACH RELEASE_CHECKLIST ROADMAP PILOT_INTAKE XLSX; do
  [[ -f "docs/$doc.md" ]] && cp "docs/$doc.md" "$STAGE/docs/"
done
mkdir -p "$STAGE/scripts"
cp scripts/demo_end_to_end.sh scripts/mcp_client.py scripts/pilot_report.py scripts/bench_mcp_cache.py scripts/smoke_http.py scripts/install_skill.sh "$STAGE/"
cp scripts/agent_routes_smoke.py scripts/check_agent_stats.py "$STAGE/scripts/"
cp docs/QUICKSTART_AGENT.md "$STAGE/AGENT_QUICKSTART.md"
mkdir -p "$STAGE/mcp_sdk" "$STAGE/skills/csvx-data" "$STAGE/examples/agent"
cp skills/csvx-data/SKILL.md "$STAGE/skills/csvx-data/"
cp examples/agent/README.md examples/agent/*.sh "$STAGE/examples/agent/"
cp examples/try_csvx.sh "$STAGE/examples/try_csvx.sh"
cp examples/try_csvx.sh "$STAGE/TRY_CSVX.sh"
cp scripts/mcp_sdk/client.mjs scripts/mcp_sdk/package.json "$STAGE/mcp_sdk/"
tar -czf "dist/$NAME.tar.gz" -C dist "$NAME"
shasum -a 256 "dist/$NAME.tar.gz" > "dist/$NAME.tar.gz.sha256"
TAR_SHA=$(shasum -a 256 "dist/$NAME.tar.gz" | awk '{print $1}')
if ! { printf '# date: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"; printf '# sha256: %s\n' "$TAR_SHA"; bash scripts/verify_dist.sh "dist/$NAME.tar.gz"; } 2>&1 | tee "dist/$NAME.verify.log"; then
  echo "package verification failed; removing incomplete artifacts" >&2
  rm -f "dist/$NAME.tar.gz" "dist/$NAME.tar.gz.sha256"
  exit 1
fi
python3 scripts/release_manifest.py "$NAME" "dist/$NAME.tar.gz" "dist/$NAME.verify.log"
python3 scripts/check_release.py "dist/$NAME.release.json"
du -h "dist/$NAME.tar.gz"
cat "dist/$NAME.tar.gz.sha256"
