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

crate() { echo "${1:-csvx-core}"; }

case "${1:-check}" in
  check)
    time cargo check -p "$(crate "${2:-}")" --all-targets
    ;;
  test)
    shift
    time cargo test --quiet "$@"
    ;;
  gate)
    # Every stage names itself when it starts and again when it ends. A gate that only
    # prints "passed" leaves the next person grepping logs to find out which of five
    # commands failed; the failing stage can say so itself.
    run_stage() {
      local name="$1"
      shift
      printf '== %s ==\n' "$name"
      if ! "$@"; then
        printf '== stage %s failed ==\n' "$name" >&2
        return 1
      fi
      printf '== %s ok ==\n' "$name"
    }
    run_stage fmt cargo fmt --all -- --check
    # A deliberately failing stage, so the naming can be tested without breaking a build.
    if [ "${GATE_FAIL_TEST:-0}" = "1" ]; then
      run_stage selftest false || exit 1
    fi
    run_stage clippy-core \
      cargo clippy --workspace --all-targets --features sql -p csvx-core -- -D warnings
    run_stage clippy-cli \
      cargo clippy --workspace --all-targets -p csvx-cli -- -D warnings
    # The workspace suite runs the 168 HTTP tests; on a loaded machine it has flaked
    # once (the assertions were never weakened). Retry once, visibly, like CI does.
    if ! run_stage tests cargo test --workspace --quiet; then
      printf '== warning: tests failed once; retrying (known load flake) ==\n' >&2
      run_stage tests-retry cargo test --workspace --quiet || exit 1
    fi
    # `sql` is an optional feature of core, and the CLI turns it on. Testing the
    # workspace alone builds core *without* it, so core's own SQL tests — the read-only
    # guard, the query wrapper — used to be skipped by the gate entirely.
    run_stage tests-sql cargo test -p csvx-core --features sql --quiet
    VERIFY_BIN=target/release/csvx
    [ -x "$VERIFY_BIN" ] || VERIFY_BIN=target/debug/csvx
    if [ -x "$VERIFY_BIN" ]; then
      run_stage verify-negative env BIN="$VERIFY_BIN" bash scripts/verify_dist_negative.sh
      run_stage stats-negative bash scripts/check_agent_stats_negative.sh
      run_stage phase-negative bash scripts/check_phase_times_negative.sh
    else
      printf '== verify-negative skipped: no release/debug binary ==\n'
    fi
    echo "== gate passed =="
    ;;
  *)
    echo "usage: $0 check [crate] | test <cargo test args> | gate" >&2
    exit 2
    ;;
esac
