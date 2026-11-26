#!/usr/bin/env bash
# Build a local diagnostic tarball. Raw CSV rows and argument values are excluded.
set -euo pipefail
cd "$(dirname "$0")/.."
PREFIX="${CSVX_INSTALL_PREFIX:-}"
ROOT="${CSVX_OPS_ROOT:-.}"
OUT="${CSVX_OPS_BUNDLE_OUT:-ops-bundle.tar.gz}"
INCLUDE_LOGS=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --root) ROOT="${2:?--root needs a directory}"; shift 2 ;;
    --out) OUT="${2:?--out needs a file}"; shift 2 ;;
    --include-logs) INCLUDE_LOGS=1; shift ;;
    -h|--help) echo "usage: bash scripts/ops_bundle.sh [--prefix DIR] [--root DIR] [--out FILE] [--include-logs]" >&2; exit 0 ;;
    *) echo "error: unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ -d "$ROOT" ]] || { echo "error: root not found: $ROOT" >&2; exit 2; }
ROOT="$(cd "$ROOT" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cp README.md "$WORK/README.md" 2>/dev/null || true
{
  printf 'csvx diagnostic bundle\n'
  printf 'created: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'root: %s\n' "$ROOT"
  printf 'prefix: %s\n' "${PREFIX:-'(none)'}"
  printf 'policy: no raw CSV rows; no argument values; no secrets\n'
  printf 'includeLogs: %s\n' "$INCLUDE_LOGS"
} > "$WORK/BUNDLE.txt"
{ uname -a; command -v csvx >/dev/null 2>&1 && csvx --version || true; } > "$WORK/environment.txt"
if [[ -n "$PREFIX" ]]; then
  bash scripts/install_selfcheck.sh --prefix "$PREFIX" > "$WORK/selfcheck.json" 2>/dev/null || true
fi
bash scripts/ops_status.sh --prefix "$PREFIX" --root "$ROOT" > "$WORK/ops-status.json" 2>/dev/null || true
mkdir -p "$WORK/state"
cp "$ROOT/.csvx/mcp-audit.jsonl" "$WORK/state/" 2>/dev/null || true
cp "$ROOT/.csvx/pipeline-history.jsonl" "$WORK/state/" 2>/dev/null || true
rm -rf "$WORK/state"
if [[ "$INCLUDE_LOGS" -eq 1 ]]; then
  mkdir -p "$WORK/state"
  cp "$ROOT/.csvx/mcp-audit.jsonl" "$WORK/state/mcp-audit.jsonl" 2>/dev/null || true
  cp "$ROOT/.csvx/pipeline-history.jsonl" "$WORK/state/pipeline-history.jsonl" 2>/dev/null || true
fi
if [[ -f "$PREFIX/install-manifest.json" ]]; then
  cp "$PREFIX/install-manifest.json" "$WORK/install-manifest.json"
fi
if [[ -f "$PREFIX/release-manifest.json" ]]; then
  cp "$PREFIX/release-manifest.json" "$WORK/release-manifest.json"
fi
OUT_DIR="$(dirname "$OUT")"
mkdir -p "$OUT_DIR"
tar -czf "$OUT" -C "$WORK" .
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$OUT" > "$OUT.sha256"
else
  sha256sum "$OUT" > "$OUT.sha256"
fi
printf 'ops_bundle=ok path=%s\n' "$OUT"
cat "$OUT.sha256"
