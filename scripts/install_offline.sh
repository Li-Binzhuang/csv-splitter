#!/usr/bin/env bash
# Install a csvx release tarball without touching the network.
# Checksum -> extract -> version/demo smoke -> install binary/docs -> manifest.
set -euo pipefail
cd "$(dirname "$0")/.."

ARCHIVE=""
PREFIX="${HOME}/.local"
SHA256=""
REQUIRE_VERSION=""
MANIFEST=""
FORCE=0

usage() {
  cat <<'USAGE'
usage: bash scripts/install_offline.sh --archive <csvx.tar.gz> [options]

  --archive PATH       release tarball (required)
  --prefix DIR         install prefix (default: $HOME/.local)
  --sha256 HEX         expected checksum (default: read <archive>.sha256)
  --manifest PATH      also verify the release manifest (sha256/bytes)
  --require-version V  refuse unless the packaged binary reports V
  --force              allow replacing a different existing manifest
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive) ARCHIVE="${2:?--archive needs a file}"; shift 2 ;;
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --sha256) SHA256="${2:?--sha256 needs a hex digest}"; shift 2 ;;
    --manifest) MANIFEST="${2:?--manifest needs a file}"; shift 2 ;;
    --require-version) REQUIRE_VERSION="${2:?--require-version needs a version}"; shift 2 ;;
    --force) FORCE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ -n "$ARCHIVE" && -f "$ARCHIVE" ]] || { echo "error: --archive must name a file" >&2; exit 2; }
ARCHIVE_ABS="$(cd "$(dirname "$ARCHIVE")" && pwd)/$(basename "$ARCHIVE")"

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

if [[ -n "$SHA256" ]]; then
  EXPECTED="$SHA256"
elif [[ -f "$ARCHIVE_ABS.sha256" ]]; then
  EXPECTED="$(awk '{print $1}' "$ARCHIVE_ABS.sha256")"
else
  echo "error: missing checksum; pass --sha256 or place $ARCHIVE_ABS.sha256" >&2
  exit 2
fi
ACTUAL="$(sha256_file "$ARCHIVE_ABS")"
if [[ "$EXPECTED" != "$ACTUAL" ]]; then
  echo "error: sha256 mismatch (expected $EXPECTED, got $ACTUAL)" >&2
  exit 1
fi
if [[ -n "$MANIFEST" ]]; then
  [[ -f "$MANIFEST" ]] || { echo "error: manifest not found: $MANIFEST" >&2; exit 2; }
  BYTES="$(wc -c < "$ARCHIVE_ABS" | tr -d ' ')"
  python3 - "$MANIFEST" "$ACTUAL" "$BYTES" <<'PY'
import json
import sys
payload = json.load(open(sys.argv[1]))
if payload.get("sha256") != sys.argv[2]:
    raise SystemExit("manifest sha256 mismatch")
if payload.get("bytes") != int(sys.argv[3]):
    raise SystemExit("manifest bytes mismatch")
PY
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
tar -xzf "$ARCHIVE_ABS" -C "$TMP"
ROOT="$(find "$TMP" -maxdepth 1 -mindepth 1 -type d | head -1)"
[[ -n "$ROOT" && -x "$ROOT/csvx" ]] || { echo "error: archive has no csvx binary" >&2; exit 1; }
VERSION="$("$ROOT/csvx" --version | awk '{print $2}')"
if [[ -n "$REQUIRE_VERSION" && "$VERSION" != "$REQUIRE_VERSION" ]]; then
  echo "error: version mismatch: archive has $VERSION, required $REQUIRE_VERSION" >&2
  exit 1
fi

"$ROOT/csvx" demo -o "$TMP/demo" >/dev/null
test -s "$TMP/demo/orders.csv" || { echo "error: demo smoke produced no orders.csv" >&2; exit 1; }

mkdir -p "$PREFIX"
PREFIX="$(cd "$PREFIX" && pwd)"
if [[ -f "$PREFIX/install-manifest.json" && "$FORCE" -ne 1 ]]; then
  if ! python3 - "$PREFIX/install-manifest.json" "$VERSION" "$ACTUAL" <<'PY'
import json
import sys
old = json.load(open(sys.argv[1]))
if old.get("version") != sys.argv[2] or old.get("sha256") != sys.argv[3]:
    raise SystemExit(1)
PY
  then
    echo "error: existing install-manifest.json has a different version/checksum; pass --force to replace" >&2
    exit 1
  fi
fi

mkdir -p "$PREFIX/bin" "$PREFIX/share/csvx"
install -m 0755 "$ROOT/csvx" "$PREFIX/bin/csvx"
for item in README.md CHANGELOG.md LICENSE docs; do
  [[ -e "$ROOT/$item" ]] || continue
  cp -R "$ROOT/$item" "$PREFIX/share/csvx/"
done

python3 - "$PREFIX" "$ARCHIVE_ABS" "$ACTUAL" "$VERSION" <<'PY'
import datetime
import json
import pathlib
import platform
import sys
prefix = pathlib.Path(sys.argv[1])
manifest = {
    "version": sys.argv[4],
    "sha256": sys.argv[3],
    "archive": sys.argv[2],
    "installedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platform": f"{platform.system()}-{platform.machine()}",
    "binary": str(prefix / "bin/csvx"),
}
(prefix / "install-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY

"$PREFIX/bin/csvx" --version
"$PREFIX/bin/csvx" demo -o "$TMP/installed-demo" >/dev/null
test -s "$TMP/installed-demo/orders.csv" || { echo "error: installed demo smoke failed" >&2; exit 1; }
printf 'install_offline=ok version=%s sha256=%s prefix=%s\n' "$VERSION" "$ACTUAL" "$PREFIX"
