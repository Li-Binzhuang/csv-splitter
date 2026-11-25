#!/usr/bin/env python3
"""Fail with a named field when Agent smoke stats miss thresholds."""
import json
import sys


def check(stats):
    failures = []
    if stats.get("routes") != 12:
        failures.append(f"routes={stats.get('routes')} (want 12)")
    if stats.get("replay") != stats.get("actions"):
        failures.append(f"replay={stats.get('replay')} actions={stats.get('actions')}")
    if not stats.get("sse_ms", 10**9) < 250:
        failures.append(f"sse_ms={stats.get('sse_ms')} (want <250)")
    return failures


def main():
    if len(sys.argv) != 2:
        print("usage: check_agent_stats.py STATS_JSON", file=sys.stderr)
        return 2
    stats = json.loads(sys.argv[1])
    failures = check(stats)
    if failures:
        print("threshold mismatch: " + "; ".join(failures), file=sys.stderr)
        return 1
    print("thresholds=ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
