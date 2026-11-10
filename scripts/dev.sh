#!/usr/bin/env bash
# Feedback loop sized to the task, because a 2-minute command after every edit is
# how an evening disappears.
#
#   scripts/dev.sh check [crate]      ~3 s    types only, no linking, no test binaries
#   scripts/dev.sh test -p core --lib ~5 s   arguments go straight to cargo test
#   scripts/dev.sh gate               ~90 s  fmt + clippy + full workspace tests
#
# The rule: `check` while editing, `test` when a feature is complete, `gate` once
# per batch of commits — never after every keystroke.
set -euo pipefail
cd "$(dirname "$0")/.."

# A non-login shell does not have cargo on PATH; find it rather than assume.
if ! command -v cargo >/dev/null 2>&1; then
  for candidate in "$HOME/.cargo/bin" /usr/local/bin /opt/homebrew/bin; do
    if [ -x "$candidate/cargo" ]; then
      PATH="$candidate:$PATH"
      export PATH
      break
    fi
  done
fi
command -v cargo >/dev/null 2>&1 || {
  echo "cargo not found on PATH" >&2
  exit 2
}

crate() { echo "${1:-csv-splitter-core}"; }

case "${1:-check}" in
  check)
    time cargo check -p "$(crate "${2:-}")" --all-targets
    ;;
  test)
    shift
    time cargo test --quiet "$@"
    ;;
  gate)
    echo "== fmt =="
    cargo fmt --all -- --check
    echo "== clippy =="
    cargo clippy --workspace --all-targets --features sql -p csv-splitter-core -- -D warnings
    cargo clippy --workspace --all-targets -p csv-splitter-cli -- -D warnings
    echo "== tests =="
    cargo test --workspace --quiet
    # `sql` is an optional feature of core, and the CLI turns it on. Testing the
    # workspace alone builds core *without* it, so core's own SQL tests — the read-only
    # guard, the query wrapper — used to be skipped by the gate entirely.
    echo "== tests (core with sql) =="
    cargo test -p csv-splitter-core --features sql --quiet
    echo "== gate passed =="
    ;;
  *)
    echo "usage: $0 check [crate] | test <cargo test args> | gate" >&2
    exit 2
    ;;
esac
