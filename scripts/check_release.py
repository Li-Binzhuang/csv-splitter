#!/usr/bin/env python3
"""Re-hash a release manifest's tarball and verify.json."""
import hashlib
import json
import pathlib
import sys


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    if len(sys.argv) != 2:
        print("usage: check_release.py RELEASE_JSON", file=sys.stderr)
        return 2
    manifest_path = pathlib.Path(sys.argv[1])
    manifest = json.loads(manifest_path.read_text())
    base = manifest_path.parent
    tar = base / f"{manifest['name']}.tar.gz"
    verify = base / manifest["verifyJson"] if manifest.get("verifyJson") else None
    failures = []
    if not tar.exists():
        failures.append(f"missing tarball: {tar}")
    elif sha(tar) != manifest.get("sha256"):
        failures.append("tarball sha256 mismatch")
    if verify is None or not verify.exists():
        failures.append("missing verify.json")
    elif sha(verify) != manifest.get("verifyJsonSha256"):
        failures.append("verify.json sha256 mismatch")
    checks = manifest.get("checks") or {}
    if checks.get("verified") is not True:
        failures.append("verify.json is not verified")
    if checks.get("skillInstall") != "ok":
        failures.append("skillInstall is not ok")
    if failures:
        print("release check failed: " + "; ".join(failures), file=sys.stderr)
        return 1
    print("manifest=ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
