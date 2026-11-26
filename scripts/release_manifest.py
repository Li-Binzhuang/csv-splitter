#!/usr/bin/env python3
"""Write a machine-readable manifest beside a verified release tarball."""
import datetime
import hashlib
import json
import os
import pathlib
import re
import sys
import tarfile


CRITICAL_DOCS = (
    "docs/SECURITY.md",
    "docs/SUPPORT_MATRIX.md",
    "docs/DEPLOY.md",
    "docs/MIGRATION.md",
)


def critical_docs(tarball):
    found = {}
    with tarfile.open(tarball, "r:gz") as archive:
        for member in archive.getmembers():
            name = member.name
            for key in CRITICAL_DOCS:
                if name == key or name.endswith("/" + key):
                    data = archive.extractfile(member).read()
                    found[key] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    missing = [name for name in CRITICAL_DOCS if name not in found]
    if missing:
        raise SystemExit("release tarball missing critical docs: " + ", ".join(missing))
    return found


def main():
    if len(sys.argv) != 4:
        print("usage: release_manifest.py NAME TARBALL LOG", file=sys.stderr)
        return 2
    name, tarball, log = sys.argv[1], pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3])
    data = tarball.read_bytes()
    verify = log.read_text() if log.exists() else ""
    match = re.search(r"^stats_json=(.+)$", verify, re.MULTILINE)
    stats = json.loads(match.group(1)) if match else None
    gate = os.environ.get("CSVX_GATE_STATUS", "not_recorded")
    aoci_raw = os.environ.get("CSVX_AOCI_ENTRIES", "")
    aoci = int(aoci_raw) if aoci_raw.isdigit() else None
    manifest = {
        "name": name,
        "sha256": hashlib.sha256(data).hexdigest(),
        "bytes": len(data),
        "builtAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "verifyLog": str(log),
        "gate": gate,
        "aociEntries": aoci,
        "agentStats": stats,
    }
    checks_path = tarball.with_name(tarball.name.replace(".tar.gz", ".verify.json"))
    checks = json.loads(checks_path.read_text()) if checks_path.exists() else None
    manifest["verifyJson"] = checks_path.name if checks_path.exists() else None
    manifest["verifyJsonSha256"] = (
        hashlib.sha256(checks_path.read_bytes()).hexdigest() if checks_path.exists() else None
    )
    manifest["checks"] = checks
    manifest["verifyFailedAt"] = checks.get("failedAt") if checks else None
    manifest["verifyDurationMs"] = checks.get("durationMs") if checks else None
    manifest["verifyPhaseMs"] = checks.get("phaseMs") if checks else None
    manifest["criticalDocs"] = critical_docs(tarball)
    manifest["agentSseLimitMs"] = 100
    manifest["verifyPhaseLimitsMs"] = {
        "extract": 2000,
        "skill": 5000,
        "try": 3000,
        "agent": 5000,
        "xlsx": 2000,
        "trace": 2000,
    }
    out = tarball.with_name(tarball.name.replace(".tar.gz", ".release.json"))
    out.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    print(f"manifest: {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
