#!/usr/bin/env bash
# Decide what a release run should do, in a script that can be tested locally.
#
# The workflow used to compute this inline, which meant the three branches (tag push,
# dispatch with a tag, dispatch without one) had never executed until somebody published
# — the same gap that hid the Windows packaging bug. This is the logic, and
# `--self-test` runs every branch with no network and no tag.
#
#   release-decision.sh <event_name> <ref> <input_tag>
#
# Prints shell assignments (`version=…`, `do_release=…`) suitable for $GITHUB_OUTPUT.
set -euo pipefail
cd "$(dirname "$0")/.."

crate_version() {
  sed -n 's/^version = "\([0-9][^"]*\)"$/\1/p' Cargo.toml | head -1
}

decide() {
  local event="$1" ref="$2" input_tag="$3"
  local version do_release
  if [ "$event" = "push" ]; then
    case "$ref" in
      refs/tags/v*)
        version="${ref#refs/tags/}"
        do_release=true
        ;;
      *)
        echo "this workflow only releases tags starting with v; got $ref" >&2
        return 1
        ;;
    esac
  elif [ -n "$input_tag" ]; then
    version="$input_tag"
    do_release=true
  else
    version="dev"
    do_release=false
  fi

  # A tag that disagrees with Cargo.toml would publish a binary whose --version lies.
  if [ "$do_release" = "true" ]; then
    local expected="v$(crate_version)"
    if [ "$version" != "$expected" ]; then
      echo "tag $version does not match Cargo.toml ($expected)" >&2
      return 1
    fi
  fi

  echo "version=$version"
  echo "do_release=$do_release"
}

self_test() {
  local failures=0
  expect() { # expect <label> <expected-exit> <expected-output> <args...>
    local label="$1" want_status="$2" want="$3"; shift 3
    local out status
    out="$(decide "$@" 2>&1)" || status=$?
    status="${status:-0}"
    if [ "$status" != "$want_status" ]; then
      printf '  FAIL %-42s exit %s, want %s: %s\n' "$label" "$status" "$want_status" "$out"
      failures=$((failures + 1))
      return
    fi
    case "$out" in
      *"$want"*) printf '  ok   %s\n' "$label" ;;
      *) printf '  FAIL %-42s got %s, want %s\n' "$label" "$out" "$want"
         failures=$((failures + 1))
         ;;
    esac
  }

  local version_tag="v$(crate_version)"
  expect "tag push releases"            0 "do_release=true" push "refs/tags/$version_tag" ""
  expect "tag push carries the version" 0 "version=$version_tag" push "refs/tags/$version_tag" ""
  expect "dispatch without a tag dry-runs" 0 "do_release=false" workflow_dispatch "refs/heads/main" ""
  expect "dispatch dry-run is version dev" 0 "version=dev" workflow_dispatch "refs/heads/main" ""
  expect "dispatch with a tag releases" 0 "do_release=true" workflow_dispatch "refs/heads/main" "$version_tag"
  expect "a branch push is refused"     1 "only releases tags" push "refs/heads/main" ""
  expect "a mismatched tag is refused"  1 "does not match Cargo.toml" push "refs/tags/v9.9.9" ""

  if [ "$failures" -eq 0 ]; then
    echo "== release decision: all cases pass =="
  else
    echo "== $failures case(s) failed =="
    return 1
  fi
}

case "${1:-}" in
  --self-test|"")
    self_test
    ;;
  *)
    [ $# -eq 3 ] || { echo "usage: release-decision.sh [--self-test] | <event> <ref> <tag>" >&2; exit 2; }
    decide "$@"
    ;;
esac
