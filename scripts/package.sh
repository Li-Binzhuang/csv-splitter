#!/usr/bin/env bash
# Turn a built binary into the artifact a user downloads.
#
# The release workflow calls this script instead of doing the packaging inline, so the
# packaging can be exercised locally — `scripts/release-check.sh` runs it and then
# unpacks the result, which is as close to "what a user gets" as we can get without
# publishing a release.
#
#   scripts/package.sh [--build] <artifact-name> [<target-triple>]
#
# Produces <artifact>.tar.gz (or .zip on Windows) plus a .sha256 next to it, in ./dist.
set -euo pipefail
cd "$(dirname "$0")/.."

# A non-login shell does not have cargo on PATH; the same lookup dev.sh uses.
if ! command -v cargo >/dev/null 2>&1; then
  for candidate in "$HOME/.cargo/bin" /usr/local/bin /opt/homebrew/bin; do
    if [ -x "$candidate/cargo" ]; then
      PATH="$candidate:$PATH"
      export PATH
      break
    fi
  done
fi

build=0
if [ "${1:-}" = "--build" ]; then
  build=1
  shift
fi
name="${1:?usage: package.sh [--build] <artifact-name> [<target>]}"
target="${2:-}"

BIN=csv-splitter
EXT=""
case "$name" in
  *windows*) EXT=".exe" ;;
esac

mkdir -p dist staging
rm -rf staging
mkdir -p staging

if [ "$build" = "1" ]; then
  if [ -n "$target" ]; then
    cargo build --release --target "$target" -p csv-splitter-cli
  else
    cargo build --release -p csv-splitter-cli
  fi
fi

if [ -n "$target" ]; then
  SRC="target/$target/release/$BIN$EXT"
else
  SRC="target/release/$BIN$EXT"
fi
if [ ! -f "$SRC" ]; then
  echo "no binary at $SRC — build it first or pass --build" >&2
  exit 2
fi
cp "$SRC" "staging/$BIN$EXT"
chmod +x "staging/$BIN$EXT" 2>/dev/null || true

rm -f "dist/$name.tar.gz" "dist/$name.zip" "dist/$name.tar.gz.sha256" "dist/$name.zip.sha256"
if [ "$EXT" = ".exe" ]; then
  ( cd staging && zip -q "../dist/$name.zip" "$BIN$EXT" )
  asset="dist/$name.zip"
else
  ( cd staging && tar czf "../dist/$name.tar.gz" "$BIN" )
  asset="dist/$name.tar.gz"
fi

if command -v shasum >/dev/null 2>&1; then
  ( cd dist && shasum -a 256 "$(basename "$asset")" > "$(basename "$asset").sha256" )
else
  ( cd dist && sha256sum "$(basename "$asset")" > "$(basename "$asset").sha256" )
fi
echo "built $asset ($(wc -c < "$asset" | tr -d ' ') bytes)"
