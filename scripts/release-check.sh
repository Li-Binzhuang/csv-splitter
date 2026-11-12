#!/usr/bin/env bash
# "Would a user be able to run what we ship?"
#
# Packages the local release binary the way the release workflow does, unpacks it in a
# clean directory, and runs the three things the documentation promises a user can do:
#   --version, demo, serve (then /health, /spec, and a SIGTERM shutdown).
#
# This is the closest local equivalent to downloading a release artifact, and it exercises
# scripts/package.sh — the same script the workflow calls.
set -euo pipefail
cd "$(dirname "$0")/.."

ARTIFACT="csv-splitter-$(uname -s | tr 'A-Z' 'a-z')-$(uname -m)"
bash scripts/package.sh --build "$ARTIFACT" >/dev/null
ASSET="dist/$ARTIFACT.tar.gz"
[ -f "$ASSET" ] || { echo "FAIL: $ASSET missing" >&2; exit 1; }
[ -f "$ASSET.sha256" ] || { echo "FAIL: checksum missing" >&2; exit 1; }
# Verified, not merely present: a checksum nobody compares is decoration.
( cd dist && if command -v shasum >/dev/null 2>&1; then shasum -a 256 -c "$(basename "$ASSET").sha256" >/dev/null; else sha256sum -c "$(basename "$ASSET").sha256" >/dev/null; fi ) \
  || { echo "FAIL: checksum does not match the artifact" >&2; exit 1; }
printf '  ok   checksum matches\n' 

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"
tar xzf "$OLDPWD/$ASSET"
[ -x ./csv-splitter ] || { echo "FAIL: binary is not executable after unpacking" >&2; exit 1; }

printf '  ok   unpacked artifact (%s bytes)\n' "$(wc -c < "./csv-splitter" | tr -d ' ')"

VERSION="$(./csv-splitter --version)"
case "$VERSION" in
  csv-splitter*) printf '  ok   --version: %s\n' "$VERSION" ;;
  *) echo "FAIL: --version printed $VERSION" >&2; exit 1 ;;
esac

./csv-splitter demo -o demo >/dev/null 2>&1
[ -s demo/orders.csv ] || { echo "FAIL: demo produced no file" >&2; exit 1; }
printf '  ok   demo wrote %s bytes\n' "$(wc -c < demo/orders.csv | tr -d ' ')"
# The artifact's own configuration check, on a directory that exists now.
./csv-splitter serve --check --root demo --addr 127.0.0.1:0 > check.log 2>&1 || {
  cat check.log >&2; echo "FAIL: the artifact's own configuration check failed" >&2; exit 1; }
printf '  ok   serve --check\n'
./csv-splitter serve --root demo --port 8790 > serve.log 2>&1 &
SPID=$!
for _ in $(seq 1 40); do
  curl -sf "127.0.0.1:8790/health" >/dev/null 2>&1 && break
  sleep 0.25
done
HEALTH="$(curl -s "127.0.0.1:8790/health")"
SPEC="$(curl -s "127.0.0.1:8790/spec")"
ROW="$(curl -s "127.0.0.1:8790/rows?path=orders.csv&count=1")"
case "$HEALTH" in *'"ready":true'*) printf '  ok   /health ready\n' ;; *) echo "FAIL: /health $HEALTH" >&2; exit 1 ;; esac
case "$SPEC" in *'"reads"'*) printf '  ok   /spec answered\n' ;; *) echo "FAIL: /spec $SPEC" >&2; exit 1 ;; esac
case "$ROW" in *'"rowKeys"'*) printf '  ok   /rows answered with keys\n' ;; *) echo "FAIL: /rows $ROW" >&2; exit 1 ;; esac

kill -TERM "$SPID"
for _ in $(seq 1 40); do kill -0 "$SPID" 2>/dev/null || break; sleep 0.25; done
if kill -0 "$SPID" 2>/dev/null; then echo "FAIL: server did not exit on SIGTERM" >&2; exit 1; fi
printf '  ok   SIGTERM shutdown\n'

echo "== release check passed: $(basename "$ASSET") =="
