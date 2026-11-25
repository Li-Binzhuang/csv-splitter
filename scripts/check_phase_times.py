#!/usr/bin/env python3
"""Reject package verification phases that exceed their budget."""
import json
import sys

LIMITS_MS = {
    "extract": 2000,
    "skill": 3000,
    "try": 3000,
    "agent": 5000,
    "xlsx": 2000,
    "trace": 2000,
}


def main():
    if len(sys.argv) != 2:
        print("usage: check_phase_times.py VERIFY_JSON", file=sys.stderr)
        return 2
    data = json.load(open(sys.argv[1], encoding="utf-8"))
    phases = data.get("phaseMs") or {}
    failures = []
    for name, limit in LIMITS_MS.items():
        value = phases.get(name)
        if not isinstance(value, (int, float)):
            failures.append(f"{name}=missing")
        elif value > limit:
            failures.append(f"{name}={value}ms (limit {limit}ms)")
    if failures:
        print("phase threshold mismatch: " + "; ".join(failures), file=sys.stderr)
        return 1
    print("phase_thresholds=ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
