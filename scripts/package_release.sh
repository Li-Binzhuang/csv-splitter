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
for doc in ACCEPTANCE AGENT_API AGENT_TRACE QUICKSTART_AGENT CAPABILITIES FORMULA_CACHE DSH_SKILL HANDOVER NIGHT_CHECKPOINT BENCHMARKS BUSINESS DEMO PILOT PILOT_SAMPLE PILOT_CONFIRMATION SALES OUTREACH RELEASE_CHECKLIST ROADMAP PILOT_INTAKE XLSX ENTRYPOINTS MCP_COMPAT SECURITY DESIGN_PARTNER SUPPORT_MATRIX DEPLOY MIGRATION PILOT_INTERVIEW; do
  [[ -f "docs/$doc.md" ]] && cp "docs/$doc.md" "$STAGE/docs/"
done
mkdir -p "$STAGE/scripts"
cp scripts/demo_end_to_end.sh scripts/mcp_client.py scripts/pilot_report.py scripts/bench_mcp_cache.py scripts/smoke_http.py "$STAGE/scripts/"
cp scripts/install_skill.sh "$STAGE/"
cp scripts/agent_routes_smoke.py scripts/check_agent_stats.py scripts/check_phase_times.py scripts/check_mcp_sdk.sh scripts/check_mcp_schema.py scripts/check_mcp_dsh.sh scripts/install_offline.sh scripts/install_selfcheck.sh scripts/test_upgrade_rollback.sh scripts/ops_status.sh scripts/ops_bundle.sh scripts/bench_100gb.py scripts/pilot_kit.sh scripts/pilot_intake.py scripts/test_install_offline.sh scripts/test_install_selfcheck.sh scripts/test_ops_status.sh scripts/test_ops_bundle.sh scripts/test_bench_100gb.sh scripts/test_pilot_kit.sh scripts/test_pilot_intake.sh "$STAGE/scripts/"
cp docs/QUICKSTART_AGENT.md "$STAGE/AGENT_QUICKSTART.md"
cp docs/bench-100gb-2026-11-26.json "$STAGE/docs/"
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
  rm -f "dist/$NAME.tar.gz" "dist/$NAME.tar.gz.sha256" "dist/$NAME.verify.json" "dist/$NAME.verify.log" "dist/$NAME.reverify.json" "dist/$NAME.release.json"
  exit 1
fi
python3 scripts/release_manifest.py "$NAME" "dist/$NAME.tar.gz" "dist/$NAME.verify.log"
python3 scripts/check_release.py "dist/$NAME.release.json"
INSTALL_DIR=$(mktemp -d)
if ! bash scripts/install_offline.sh --archive "dist/$NAME.tar.gz" --manifest "dist/$NAME.release.json" --prefix "$INSTALL_DIR" >/dev/null; then
  rm -rf "$INSTALL_DIR"
  echo "offline install smoke failed" >&2
  exit 1
fi
"$INSTALL_DIR/bin/csvx" --version >/dev/null
if ! bash scripts/install_selfcheck.sh --prefix "$INSTALL_DIR" > "$INSTALL_DIR/selfcheck.json"; then
  cat "$INSTALL_DIR/selfcheck.json" >&2 || true
  rm -rf "$INSTALL_DIR"
  echo "installed prefix self-check failed" >&2
  exit 1
fi
python3 - "$INSTALL_DIR/selfcheck.json" <<'PYCHECK'
import json
import sys
data = json.load(open(sys.argv[1]))
if not data.get("ok"):
    raise SystemExit(data)
PYCHECK
if ! bash scripts/ops_status.sh --prefix "$INSTALL_DIR" --root "$INSTALL_DIR" > "$INSTALL_DIR/ops.json"; then
  cat "$INSTALL_DIR/ops.json" >&2 || true
  rm -rf "$INSTALL_DIR"
  echo "installed prefix ops status failed" >&2
  exit 1
fi
python3 - "$INSTALL_DIR/ops.json" <<'PYOPS'
import json
import sys
data = json.load(open(sys.argv[1]))
if not data.get("installOk"):
    raise SystemExit(data)
PYOPS
if ! bash scripts/ops_bundle.sh --prefix "$INSTALL_DIR" --root "$INSTALL_DIR" --out "$INSTALL_DIR/ops-bundle.tar.gz" > "$INSTALL_DIR/bundle.log"; then
  cat "$INSTALL_DIR/bundle.log" >&2 || true
  rm -rf "$INSTALL_DIR"
  echo "ops bundle smoke failed" >&2
  exit 1
fi
tar -tzf "$INSTALL_DIR/ops-bundle.tar.gz" | grep -q './selfcheck.json'
tar -tzf "$INSTALL_DIR/ops-bundle.tar.gz" | grep -q './ops-status.json'
rm -rf "$INSTALL_DIR"
printf 'install_manifest=ok selfcheck=ok ops_status=ok ops_bundle=ok\n'
du -h "dist/$NAME.tar.gz"
cat "dist/$NAME.tar.gz.sha256"
