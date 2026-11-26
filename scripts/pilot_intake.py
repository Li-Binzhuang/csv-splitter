#!/usr/bin/env python3
"""Validate a design-partner sample without reading raw rows into reports."""
import argparse
import csv
import gzip
import json
import os
import re
import sys
from pathlib import Path

SENSITIVE = {
    "email": re.compile(r"e-?mail", re.I),
    "phone": re.compile(r"phone|mobile|cell", re.I),
    "ssn": re.compile(r"ssn|social[_-]?security", re.I),
    "name": re.compile(r"full[_-]?name|first[_-]?name|last[_-]?name", re.I),
    "address": re.compile(r"address|street", re.I),
}
GB = 1_000_000_000


def open_text(path):
    if path.suffix.lower() == ".gz":
        return gzip.open(path, "rt", encoding="utf-8", errors="replace")
    return path.open("r", encoding="utf-8", errors="replace")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sample", default=os.environ.get("CSVX_PILOT_FILE", ""))
    parser.add_argument("--out", default="pilot-intake")
    parser.add_argument("--max-gb", type=float, default=20.0)
    args = parser.parse_args()
    if not args.sample:
        print("error: set --sample or CSVX_PILOT_FILE", file=sys.stderr)
        return 2
    sample = Path(args.sample)
    if not sample.is_file():
        print(f"error: sample not found: {sample}", file=sys.stderr)
        return 2
    size = sample.stat().st_size
    limit = args.max_gb * GB
    within = size <= limit
    with open_text(sample) as handle:
        reader = csv.reader(handle)
        header = next(reader, [])
    warnings = []
    for column in header:
        for label, pattern in SENSITIVE.items():
            if pattern.search(column):
                warnings.append({"column": column, "kind": label})
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    payload = {
        "sample": str(sample.resolve()),
        "bytes": size,
        "gb": round(size / GB, 3),
        "maxGb": args.max_gb,
        "withinLimit": within,
        "columns": header,
        "columnCount": len(header),
        "sensitiveWarnings": warnings,
        "readRawRows": False,
    }
    (out / "intake.json").write_text(json.dumps(payload, indent=2) + "\n")
    md = [
        "# csvx pilot intake",
        "",
        f"- sample: `{payload['sample']}`",
        f"- size: {size} bytes ({payload['gb']} GB)",
        f"- within {args.max_gb} GB: {within}",
        f"- columns: {len(header)}",
        "",
        "## Column names",
        "",
    ]
    md += [f"- `{column}`" for column in header] or ["- (no header found)"]
    md += ["", "## Possible sensitive columns", ""]
    md += [f"- `{item['column']}` ({item['kind']})" for item in warnings] or ["- none detected by name"]
    md += [
        "",
        "## Checklist",
        "",
        f"- [{'ready' if within else 'partner'}] size limit",
        "- [partner] one concrete question and acceptance metric",
        "- [partner] baseline command and expected result",
        "- [human] deletion date and go/no-go",
        "",
        "No raw row values are written to this report. No network access.",
        "",
    ]
    (out / "intake.md").write_text("\n".join(md) + "\n")
    print(f"pilot_intake={'ok' if within else 'too_large'} file={sample} bytes={size} columns={len(header)} warnings={len(warnings)}")
    print(f"intake_json={out / 'intake.json'}")
    return 0 if within else 1


if __name__ == "__main__":
    raise SystemExit(main())
