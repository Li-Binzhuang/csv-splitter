#!/usr/bin/env bash
# "Do the three documented install paths actually produce a working binary?"
#
#   release artifact  — covered by scripts/release-check.sh (unpacks the tarball)
#   cargo install     — verified here: installs into a throwaway prefix, then runs it
#   source build      — verified here: builds the release binary and runs it
#
# Each path ends with the same smoke: --version, demo, and serve --check on the result.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  for candidate in "$HOME/.cargo/bin" /usr/local/bin /opt/homebrew/bin; do
    if [ -x "$candidate/cargo" ]; then
      PATH="$candidate:$PATH"
      export PATH
      break
    fi
  done
fi
command -v cargo >/dev/null 2>&1 || { echo "cargo not found on PATH" >&2; exit 2; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

probe() { # probe <label> <binary>
  local label="$1" bin="$2"
  local version
  version="$("$bin" --version)"
  case "$version" in
    csv-splitter*) printf '  ok   %-14s %s\n' "$label" "$version" ;;
    *) echo "FAIL: $label printed $version" >&2; exit 1 ;;
  esac
  "$bin" demo -o "$WORK/demo-$label" >/dev/null 2>&1
  [ -s "$WORK/demo-$label/orders.csv" ] || { echo "FAIL: $label demo wrote nothing" >&2; exit 1; }
  "$bin" serve --check --root "$WORK/demo-$label" --addr 127.0.0.1:0 > "$WORK/check-$label.log" 2>&1 || {
    cat "$WORK/check-$label.log" >&2
    echo "FAIL: $label serve --check failed" >&2
    exit 1
  }
  printf '  ok   %-14s demo + serve --check\n' "$label"
}

echo "== source build =="
cargo build --release -p csv-splitter-cli --quiet
probe "source" "$PWD/target/release/csv-splitter"

echo "== cargo install --path =="
PREFIX="$WORK/prefix"
# Offline first: the dependency cache is warm from the build above, and reaching for the
# registry is what makes this step fail on a slow network. A clean machine has no cache,
# so fall back to a normal install once and say so.
if ! cargo install --path crates/cli --root "$PREFIX" --offline --quiet 2>&1 | tail -3; then
  echo "  note installing with network access (the dependency cache was empty)"
  cargo install --path crates/cli --root "$PREFIX" --quiet 2>&1 | tail -3 || {
    echo "FAIL: cargo install --path crates/cli failed" >&2
    exit 1
  }
fi
probe "cargo-install" "$PREFIX/bin/csv-splitter"

echo "== release artifact =="
bash scripts/release-check.sh 2>&1 | grep -E "^  ok|^==|^FAIL" | sed 's/^/  /'

echo "== install check passed (3 paths) =="
