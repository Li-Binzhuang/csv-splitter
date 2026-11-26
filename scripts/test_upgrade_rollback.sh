#!/usr/bin/env bash
# Simulate old/new prefixes, switch a current symlink and roll it back.
set -euo pipefail
cd "$(dirname "$0")/.."
REAL="${CSVX_BIN:-target/debug/csvx}"
[[ -x "$REAL" ]] || REAL=target/release/csvx
[[ -x "$REAL" ]] || { echo "upgrade_rollback=fail no binary" >&2; exit 2; }
REAL="$(cd "$(dirname "$REAL")" && pwd)/$(basename "$REAL")"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PLATFORM="$(uname -s)-$(uname -m)"

mkprefix() {
  local dir="$1" version="$2"
  mkdir -p "$dir/bin" "$dir/share/csvx/docs"
  cat > "$dir/bin/csvx" <<EOF
#!/bin/sh
if [ "\$1" = "--version" ]; then
  echo "csvx $version (DuckDB: compiled in)"
  exit 0
fi
exec "$REAL" "\$@"
EOF
  chmod +x "$dir/bin/csvx"
  cp docs/SECURITY.md docs/MIGRATION.md "$dir/share/csvx/docs/"
  python3 - "$dir/install-manifest.json" "$version" "$PLATFORM" <<'PY'
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
}

sc() { bash scripts/install_selfcheck.sh --prefix "$1"; }

OLD="$WORK/prefix-old"
NEW="$WORK/prefix-new"
mkprefix "$OLD" "4.1.0"
mkprefix "$NEW" "4.2.0"
sc "$OLD" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["ok"] and d["version"]=="4.1.0", d' || { echo "upgrade_rollback=fail old selfcheck" >&2; exit 1; }
sc "$NEW" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["ok"] and d["version"]=="4.2.0", d' || { echo "upgrade_rollback=fail new selfcheck" >&2; exit 1; }

ln -s "$OLD" "$WORK/current"
sc "$WORK/current" >/dev/null
ln -sfn "$NEW" "$WORK/current"
sc "$WORK/current" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["ok"] and d["version"]=="4.2.0", d' || { echo "upgrade_rollback=fail switch" >&2; exit 1; }
ln -sfn "$OLD" "$WORK/current"
sc "$WORK/current" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["ok"] and d["version"]=="4.1.0", d' || { echo "upgrade_rollback=fail rollback" >&2; exit 1; }

rm -rf "$OLD"
if sc "$OLD" >/dev/null 2>&1; then
  echo "upgrade_rollback=fail missing prefix accepted" >&2
  exit 1
fi
printf 'upgrade_rollback=ok old=4.1.0 new=4.2.0 switch=ok rollback=ok missing_prefix=rejected\n'
