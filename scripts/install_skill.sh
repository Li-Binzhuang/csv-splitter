#!/usr/bin/env bash
# Install the csvx skill into the local DSH profile (repo or packaged layout).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
if [[ -f "$here/skills/csvx-data/SKILL.md" ]]; then
  src="$here/skills/csvx-data/SKILL.md"
elif [[ -f "$here/../skills/csvx-data/SKILL.md" ]]; then
  src="$here/../skills/csvx-data/SKILL.md"
else
  echo "missing skills/csvx-data/SKILL.md (looked in $here and $here/..)" >&2
  exit 2
fi
home=${DSH_HOME:-"$HOME/.dsh"}
dest="$home/skills/csvx-data"
mkdir -p "$dest"
cp "$src" "$dest/SKILL.md"
echo "installed: $dest/SKILL.md"
