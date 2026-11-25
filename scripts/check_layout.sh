#!/usr/bin/env bash
# Enforce the entrypoint layout rules in docs/ENTRYPOINTS.md.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() {
  echo "layout error: $*" >&2
  exit 1
}

[[ -f docs/ENTRYPOINTS.md ]] || fail "missing docs/ENTRYPOINTS.md"

ROOT_ALLOWED=(
  .DS_Store .aoci .codegraph .git .gitattributes .github .gitignore
  AGENTS.md CHANGELOG.md Cargo.lock Cargo.toml LICENSE README.md
  aoci.code.txt aoci.meta.txt aoci.txt
  crates csvx-demo deploy dist docs examples scripts skills staging target
  install_skill.sh TRY_CSVX.sh
)
while IFS= read -r entry; do
  name=${entry#./}
  allowed=0
  for candidate in "${ROOT_ALLOWED[@]}"; do
    [[ "$name" == "$candidate" ]] && allowed=1 && break
  done
  [[ "$allowed" == 1 ]] || fail "unexpected root entry: $name"
done < <(find . -maxdepth 1 -mindepth 1 -print | sort)

if find . -maxdepth 1 -name '*.rs' -print -quit | grep -q .; then
  fail "Rust source at repository root"
fi

CRATE_ALLOWED=(core cli mcp agent api)
for dir in crates/*/; do
  name=$(basename "$dir")
  allowed=0
  for candidate in "${CRATE_ALLOWED[@]}"; do
    [[ "$name" == "$candidate" ]] && allowed=1 && break
  done
  [[ "$allowed" == 1 ]] || fail "unexpected crate: crates/$name"
  [[ -f "crates/$name/Cargo.toml" ]] || fail "crates/$name has no Cargo.toml"
done

CLI_SRC_ALLOWED=(main.rs serve.rs cli_support.rs)
CLI_COMMANDS_ALLOWED=(mod.rs agent.rs analyze.rs book.rs cache.rs chart.rs columns.rs concat.rs convert.rs dedupe.rs demo.rs edit.rs extract.rs files.rs filter.rs format.rs gate.rs groupby.rs index.rs layout.rs lookup.rs mcp.rs merge.rs ops.rs partition.rs pipeline.rs pivot.rs preview.rs repair.rs report.rs schema.rs search.rs serve.rs shared.rs sort.rs split.rs sql.rs stats.rs validate.rs verify.rs view.rs)
while IFS= read -r entry; do
  name=${entry#crates/cli/src/}
  allowed=0
  for candidate in "${CLI_SRC_ALLOWED[@]}"; do
    [[ "$name" == "$candidate" ]] && allowed=1 && break
  done
  [[ "$allowed" == 1 ]] || fail "unexpected CLI entry file: crates/cli/src/$name (update docs/ENTRYPOINTS.md first)"
done < <(find crates/cli/src -maxdepth 1 -name '*.rs' -print | sort)

while IFS= read -r entry; do
  name=${entry#crates/cli/src/commands/}
  allowed=0
  for candidate in "${CLI_COMMANDS_ALLOWED[@]}"; do
    [[ "$name" == "$candidate" ]] && allowed=1 && break
  done
  [[ "$allowed" == 1 ]] || fail "unexpected command module: crates/cli/src/commands/$name (update docs/ENTRYPOINTS.md first)"
done < <(find crates/cli/src/commands -maxdepth 1 -name '*.rs' -print | sort)

if grep -q 'csvx-cli' crates/core/Cargo.toml; then
  fail "core must not depend on the CLI"
fi

line_count=$(wc -l < crates/cli/src/main.rs)
if (( line_count > 2000 )); then
  echo "layout warning: crates/cli/src/main.rs is ${line_count} lines; split it instead of growing it" >&2
fi

echo "layout=ok"
