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
for required in csvx README.md docs/RELEASE_CHECKLIST.md docs/ROADMAP.md mcp_sdk/client.mjs; do
  if [[ ! -f "$ROOT/$required" ]]; then
    echo "missing member: $required" >&2
    exit 1
  fi
done
"$ROOT/csvx" --version
echo "verified: $TAR"
