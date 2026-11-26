#!/usr/bin/env python3
"""Validate a design-partner sample without reading raw rows into reports."""
import argparse
import bz2
import csv
import gzip
import json
import os
import re
import subprocess
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


def read_header(path):
    suffix = path.suffix.lower()
    if suffix == ".zst":
        proc = subprocess.Popen(
            ["zstd", "-dc", str(path)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        first = proc.stdout.readline() if proc.stdout else ""
        proc.terminate()
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            proc.kill()
        if proc.returncode not in (0, -15) and not first:
            raise OSError("zstd decompression failed")
        return next(csv.reader([first]), [])
    if suffix == ".bz2":
        handle = bz2.open(path, "rt", encoding="utf-8", errors="replace")
    elif suffix == ".gz":
        handle = gzip.open(path, "rt", encoding="utf-8", errors="replace")
    else:
        handle = path.open("r", encoding="utf-8", errors="replace")
    with handle as stream:
        reader = csv.reader(stream)
        return next(reader, [])


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
    error = None
    try:
        header = read_header(sample)
        if not header:
            error = "empty header"
    except (csv.Error, OSError, UnicodeError) as exc:
        header = []
        error = f"cannot read header: {exc}"
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
        "error": error,
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
    state = "invalid" if error else ("ok" if within else "too_large")
    print(f"pilot_intake={state} file={sample} bytes={size} columns={len(header)} warnings={len(warnings)}")
    print(f"intake_json={out / 'intake.json'}")
    if error:
        return 2
    return 0 if within else 1


if __name__ == "__main__":
    raise SystemExit(main())
