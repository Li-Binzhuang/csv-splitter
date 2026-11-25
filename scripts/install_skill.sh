#!/usr/bin/env bash
# Install the csvx skill into the local DSH profile.
set -euo pipefail
cd "$(dirname "$0")/.."
home=${DSH_HOME:-"$HOME/.dsh"}
dest="$home/skills/csvx-data"
if [[ ! -f skills/csvx-data/SKILL.md ]]; then
  echo "missing skills/csvx-data/SKILL.md" >&2
  exit 2
fi
mkdir -p "$dest"
cp skills/csvx-data/SKILL.md "$dest/SKILL.md"
echo "installed: $dest/SKILL.md"
