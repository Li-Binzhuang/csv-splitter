#!/usr/bin/env bash
# Smoke the release manifest's critical-doc binding and its negative.
set -euo pipefail
cd "$(dirname "$0")/.."
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
NAME="csvx-docs-test"
STAGE="$WORK/stage/$NAME"
mkdir -p "$STAGE/docs"
cp docs/SECURITY.md docs/SUPPORT_MATRIX.md docs/DEPLOY.md docs/MIGRATION.md "$STAGE/docs/"
printf 'placeholder\n' > "$STAGE/csvx"
tar -czf "$WORK/$NAME.tar.gz" -C "$WORK/stage" "$NAME"
cat > "$WORK/$NAME.verify.json" <<'JSON'
{"verified": true, "skillInstall": "ok", "phaseThresholds": "ok"}
JSON
: > "$WORK/verify.log"
python3 scripts/release_manifest.py "$NAME" "$WORK/$NAME.tar.gz" "$WORK/verify.log" >/dev/null
python3 - "$WORK/$NAME.release.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert sorted(data["criticalDocs"]) == ["docs/DEPLOY.md", "docs/MIGRATION.md", "docs/SECURITY.md", "docs/SUPPORT_MATRIX.md"], data
print("release_manifest=critical_docs")
PY
python3 scripts/check_release.py "$WORK/$NAME.release.json" >/dev/null

BAD="$WORK/stage-bad/$NAME"
mkdir -p "$BAD/docs"
cp docs/SECURITY.md docs/SUPPORT_MATRIX.md docs/MIGRATION.md "$BAD/docs/"
printf 'placeholder\n' > "$BAD/csvx"
tar -czf "$WORK/bad-$NAME.tar.gz" -C "$WORK/stage-bad" "$NAME"
if python3 scripts/release_manifest.py "$NAME" "$WORK/bad-$NAME.tar.gz" "$WORK/verify.log" >/dev/null 2>&1; then
  echo "release_manifest=fail missing doc accepted" >&2
  exit 1
fi
echo "release_manifest=missing_doc_rejected"
