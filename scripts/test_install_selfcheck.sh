#!/usr/bin/env bash
# Positive/negative checks for install_selfcheck.sh without building a release.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${CSVX_BIN:-target/debug/csvx}"
[[ -x "$BIN" ]] || BIN=target/release/csvx
[[ -x "$BIN" ]] || { echo "install_selfcheck=fail no binary" >&2; exit 2; }
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PREFIX="$WORK/prefix"
mkdir -p "$PREFIX/bin" "$PREFIX/share/csvx/docs"
cp "$BIN" "$PREFIX/bin/csvx"
cp docs/SECURITY.md docs/MIGRATION.md "$PREFIX/share/csvx/docs/"
VERSION="$("$BIN" --version | awk '{print $2}')"
PLATFORM="$(uname -s)-$(uname -m)"
python3 - "$PREFIX/install-manifest.json" "$VERSION" "$PLATFORM" <<'PY'
import datetime
import json
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
path.write_text(json.dumps({
    "version": sys.argv[2],
    "sha256": "0" * 64,
    "platform": sys.argv[3],
    "installedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "binary": str(path.parent / "bin/csvx"),
}, indent=2) + "\n")
PY
OUT="$(bash scripts/install_selfcheck.sh --prefix "$PREFIX")"
printf '%s\n' "$OUT" | python3 -c 'import json,sys; data=json.load(sys.stdin); assert data["ok"] is True, data; assert all(item["ok"] for item in data["checks"]), data; print("install_selfcheck=ok")'

BAD="$WORK/bad-prefix"
cp -R "$PREFIX" "$BAD"
python3 - "$BAD/install-manifest.json" <<'PY'
import json
import sys
path = sys.argv[1]
data = json.load(open(path))
data["version"] = "0.0.0"
open(path, "w").write(json.dumps(data) + "\n")
PY
if bash scripts/install_selfcheck.sh --prefix "$BAD" >"$WORK/bad.json" 2>/dev/null; then
  echo "install_selfcheck=fail bad version accepted" >&2
  exit 1
fi
python3 - "$WORK/bad.json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
assert data["ok"] is False, data
print("install_selfcheck=bad_version_rejected")
PY

NO_DOCS="$WORK/no-docs"
cp -R "$PREFIX" "$NO_DOCS"
rm -f "$NO_DOCS/share/csvx/docs/MIGRATION.md"
if bash scripts/install_selfcheck.sh --prefix "$NO_DOCS" >/dev/null 2>&1; then
  echo "install_selfcheck=fail missing docs accepted" >&2
  exit 1
fi
echo "install_selfcheck=missing_docs_rejected"
