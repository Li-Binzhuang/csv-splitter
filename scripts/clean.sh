#!/usr/bin/env bash
# Remove what this project leaves behind: stress-test fixtures and build output.
#
# The fixtures are the dangerous part — a 1 GB file copied five times to test
# split/merge is 5 GB, and nothing reminds you it is there. This script refuses to
# guess: it only deletes paths under the temp directory that this project made,
# plus `target/` when asked.
#
#   scripts/clean.sh          fixtures only (default: fast)
#   scripts/clean.sh --all    fixtures + cargo target (slow to rebuild)
set -euo pipefail

TMPDIR_BASE="${TMPDIR:-/tmp}"
TMPDIR_BASE="${TMPDIR_BASE%/}"

echo "before:"
du -sh "$TMPDIR_BASE"/* 2>/dev/null | sort -rh | head -3 || true

removed=0
for pattern in csvs_demo csvs_* big1g.csv big10* messy.csv tour wb_partition; do
  for path in "$TMPDIR_BASE"/$pattern; do
    if [ -e "$path" ]; then
      size=$(du -sh "$path" 2>/dev/null | cut -f1)
      echo "  removing $path ($size)"
      rm -rf "$path"
      removed=1
    fi
  done
done

if [ "${1:-}" = "--all" ]; then
  echo "  cargo clean"
  cargo clean
fi

if [ "$removed" -eq 0 ] && [ "${1:-}" != "--all" ]; then
  echo "  nothing to remove"
fi

echo "after:"
df -h / 2>/dev/null | tail -1 || true
