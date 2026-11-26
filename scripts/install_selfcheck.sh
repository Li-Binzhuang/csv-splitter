#!/usr/bin/env bash
# Check an installed csvx prefix and print one JSON object. No network.
set -euo pipefail
cd "$(dirname "$0")/.."

PREFIX="${CSVX_INSTALL_PREFIX:-}"
AUDIT_DIR="${CSVX_AUDIT_DIR:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --audit-dir) AUDIT_DIR="${2:?--audit-dir needs a directory}"; shift 2 ;;
    -h|--help)
      echo "usage: bash scripts/install_selfcheck.sh --prefix DIR [--audit-dir DIR]" >&2
      exit 0
      ;;
    *) echo "error: unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$PREFIX" ]] || { echo "error: --prefix is required" >&2; exit 2; }
[[ -d "$PREFIX" ]] || { echo "error: prefix not found: $PREFIX" >&2; exit 2; }
PREFIX="$(cd "$PREFIX" && pwd)"
MANIFEST="$PREFIX/install-manifest.json"
BIN="$PREFIX/bin/csvx"
DOCS="$PREFIX/share/csvx/docs"
AUDIT_DIR="${AUDIT_DIR:-$PREFIX/var/csvx/audit}"
PLATFORM="$(uname -s)-$(uname -m)"
FAIL=0
CHECKS=()

check() {
  local name="$1" ok="$2" detail="$3"
  CHECKS+=("$name:$ok:$detail")
  [[ "$ok" == "true" ]] || FAIL=1
}

if [[ -f "$MANIFEST" ]]; then
  check manifest true "$MANIFEST"
else
  check manifest false "missing $MANIFEST"
fi
if [[ -x "$BIN" ]]; then
  check binary true "$BIN"
  VERSION="$("$BIN" --version | awk '{print $2}')"
  check binary_version true "$VERSION"
else
  check binary false "missing executable $BIN"
  VERSION=""
fi

MANIFEST_VERSION=""
MANIFEST_SHA=""
MANIFEST_PLATFORM=""
if [[ -f "$MANIFEST" ]]; then
  eval "$(python3 - "$MANIFEST" <<'PY'
import json
import shlex
import sys
data = json.load(open(sys.argv[1]))
for key, var in (("version", "MANIFEST_VERSION"), ("sha256", "MANIFEST_SHA"), ("platform", "MANIFEST_PLATFORM")):
    print(f"{var}={shlex.quote(str(data.get(key, '')))}")
PY
)"
fi
if [[ -n "$MANIFEST_VERSION" && "$MANIFEST_VERSION" == "$VERSION" ]]; then
  check manifest_version true "$VERSION"
else
  check manifest_version false "manifest=$MANIFEST_VERSION binary=$VERSION"
fi
if [[ -n "$MANIFEST_PLATFORM" && "$MANIFEST_PLATFORM" == "$PLATFORM" ]]; then
  check platform true "$PLATFORM"
else
  check platform false "manifest=$MANIFEST_PLATFORM host=$PLATFORM"
fi
if [[ -d "$DOCS" && -f "$DOCS/SECURITY.md" && -f "$DOCS/MIGRATION.md" ]]; then
  check docs true "$DOCS"
else
  check docs false "missing docs under $DOCS"
fi
if mkdir -p "$AUDIT_DIR" 2>/dev/null && touch "$AUDIT_DIR/.write-test" 2>/dev/null; then
  rm -f "$AUDIT_DIR/.write-test"
  check audit_dir true "$AUDIT_DIR"
else
  check audit_dir false "not writable: $AUDIT_DIR"
fi

python3 - "$PREFIX" "$VERSION" "$MANIFEST_VERSION" "$MANIFEST_SHA" "$PLATFORM" "$AUDIT_DIR" "$FAIL" "${CHECKS[@]}" <<'PY'
import json
import sys
prefix, version, manifest_version, sha, platform, audit, fail, *checks = sys.argv[1:]
items = []
for raw in checks:
    name, ok, detail = raw.split(":", 2)
    items.append({"name": name, "ok": ok == "true", "detail": detail})
payload = {
    "ok": fail == "0",
    "prefix": prefix,
    "version": version,
    "manifestVersion": manifest_version,
    "sha256": sha,
    "platform": platform,
    "auditDir": audit,
    "checks": items,
}
print(json.dumps(payload, indent=2, ensure_ascii=False))
raise SystemExit(0 if fail == "0" else 1)
PY
