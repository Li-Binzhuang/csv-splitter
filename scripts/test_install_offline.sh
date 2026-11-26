#!/usr/bin/env bash
# Exercise the offline installer with a real tarball and a bad-checksum negative.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${CSVX_BIN:-target/debug/csvx}"
if [[ ! -x "$BIN" ]]; then
  BIN=target/release/csvx
fi
[[ -x "$BIN" ]] || { echo "install_offline=fail no binary ($BIN)" >&2; exit 2; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STAGE="$WORK/csvx-test"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/csvx"
cp README.md LICENSE "$STAGE/" 2>/dev/null || true
tar -czf "$WORK/csvx-test.tar.gz" -C "$WORK" csvx-test
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$WORK/csvx-test.tar.gz" > "$WORK/csvx-test.tar.gz.sha256"
else
  sha256sum "$WORK/csvx-test.tar.gz" > "$WORK/csvx-test.tar.gz.sha256"
fi

PREFIX="$WORK/prefix"
SHA="$(awk '{print $1}' "$WORK/csvx-test.tar.gz.sha256")"
BYTES="$(wc -c < "$WORK/csvx-test.tar.gz" | tr -d ' ')"
printf '{"name":"csvx-test","sha256":"%s","bytes":%s}\n' "$SHA" "$BYTES" > "$WORK/release.json"
bash scripts/install_offline.sh --archive "$WORK/csvx-test.tar.gz" --prefix "$PREFIX" --manifest "$WORK/release.json" >/dev/null
[[ -x "$PREFIX/bin/csvx" ]] || { echo "install_offline=fail binary missing" >&2; exit 1; }
"$PREFIX/bin/csvx" --version >/dev/null
python3 - "$PREFIX/install-manifest.json" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1]))
for key in ("version", "sha256", "installedAt", "binary", "platform"):
    if key not in manifest:
        raise SystemExit(f"missing {key}")
print("install_manifest=ok")
PY

bad_prefix="$WORK/bad-prefix"
if bash scripts/install_offline.sh --archive "$WORK/csvx-test.tar.gz" --prefix "$bad_prefix" \
  --sha256 "0000000000000000000000000000000000000000000000000000000000000000" >/dev/null 2>&1; then
  echo "install_offline=fail bad checksum accepted" >&2
  exit 1
fi
[[ ! -f "$bad_prefix/install-manifest.json" ]] || { echo "install_offline=fail bad checksum wrote manifest" >&2; exit 1; }

printf '{"name":"csvx-test","sha256":"0000000000000000000000000000000000000000000000000000000000000000","bytes":%s}\n' "$BYTES" > "$WORK/bad-release.json"
if bash scripts/install_offline.sh --archive "$WORK/csvx-test.tar.gz" --prefix "$WORK/bad-manifest-prefix" \
  --manifest "$WORK/bad-release.json" >/dev/null 2>&1; then
  echo "install_offline=fail bad manifest accepted" >&2
  exit 1
fi

if bash scripts/install_offline.sh --archive "$WORK/csvx-test.tar.gz" --prefix "$WORK/version-prefix" \
  --require-version 0.0.0 >/dev/null 2>&1; then
  echo "install_offline=fail version mismatch accepted" >&2
  exit 1
fi

echo "install_offline=ok bad_sha=rejected bad_manifest=rejected version_lock=rejected"
