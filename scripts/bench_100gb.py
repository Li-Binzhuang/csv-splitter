#!/usr/bin/env python3
"""Generate a deterministic CSV and measure csvx scan commands.

The default target is 100 GB. A small --rows run is the smoke path; full 100 GB
runs are deliberately explicit because they take time and disk.
"""
import argparse
import json
import os
import platform
import resource
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HEADER = "id,city,amount,note\n"
NOTE = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ\n"
GB = 1_000_000_000


def line(i):
    city = "CDMX" if i % 5 == 0 else "Puebla"
    return f"{i},{city},{i % 1000},{NOTE}"


def generate_rows(path, rows):
    started = time.time()
    with path.open("w", buffering=1024 * 1024) as out:
        out.write(HEADER)
        written = len(HEADER)
        for i in range(1, rows + 1):
            text = line(i)
            out.write(text)
            written += len(text)
    return written, rows, time.time() - started


def generate_bytes(path, target):
    started = time.time()
    block_rows = 100_000
    block = "".join(line(i) for i in range(1, block_rows + 1)).encode()
    rows = 0
    with path.open("wb", buffering=1024 * 1024) as out:
        out.write(HEADER.encode())
        written = len(HEADER)
        while written + len(block) <= target:
            out.write(block)
            written += len(block)
            rows += block_rows
        # Finish with complete lines close to the requested byte target.
        i = 1
        while written < target:
            text = line(i).encode()
            if written + len(text) > target:
                break
            out.write(text)
            written += len(text)
            rows += 1
            i += 1
    return written, rows, time.time() - started


def find_time():
    return "/usr/bin/time" if Path("/usr/bin/time").exists() else None


def run_measured(binary, args):
    time_bin = find_time()
    if time_bin:
        system = platform.system()
        flag = "-l" if system == "Darwin" else "-v"
        command = [time_bin, flag, binary, *args]
        proc = subprocess.run(
            command, capture_output=True, text=True, timeout=None, check=False
        )
        rss = 0
        for line in proc.stderr.splitlines():
            lower = line.lower()
            if "maximum resident set size" in lower:
                digits = "".join(ch for ch in line if ch.isdigit())
                if digits:
                    rss = int(digits)
                    if system != "Darwin" and "(kbytes)" in lower:
                        rss *= 1024
                break
        stderr = proc.stderr
    else:
        started = time.time()
        proc = subprocess.run([binary, *args], capture_output=True, text=True, check=False)
        stderr = proc.stderr
        rss = 0
    elapsed = proc.returncode
    if elapsed != 0:
        return {
            "args": args,
            "exit": proc.returncode,
            "error": (proc.stderr or proc.stdout)[-2000:],
            "elapsedMs": 0,
            "rssBytes": rss,
        }
    status = proc.stdout[-2000:]
    # Wall time is measured separately for a stable value independent of time(1).
    return {"args": args, "exit": 0, "outputTail": status, "rssBytes": rss}


def measure(binary, args):
    # Run once with time(1) for RSS, and use a monotonic clock for elapsed.
    started = time.time()
    result = run_measured(binary, args)
    result["elapsedMs"] = round((time.time() - started) * 1000, 1)
    return result


def human_bytes(value):
    if value >= GB:
        return f"{value / GB:.2f} GB"
    if value >= 1_000_000:
        return f"{value / 1_000_000:.2f} MB"
    return f"{value} bytes"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gb", type=float, default=100.0, help="target file size (default 100)")
    parser.add_argument("--rows", type=int, default=0, help="exact rows; overrides --gb")
    parser.add_argument("--out", default="bench/orders.csv", help="CSV path")
    parser.add_argument("--binary", default=os.environ.get("CSVX_BIN", "target/release/csvx"))
    parser.add_argument("--json-out", default="", help="default: <out>.bench.json")
    parser.add_argument("--md-out", default="", help="default: <out>.bench.md")
    parser.add_argument("--keep", action="store_true", help="keep the generated CSV (default)")
    parser.add_argument("--clean", action="store_true", help="delete the CSV after the run")
    args = parser.parse_args()

    binary = Path(args.binary)
    if not binary.exists():
        found = shutil.which(args.binary)
        if not found:
            print(f"bench: binary not found: {args.binary}", file=sys.stderr)
            return 2
        binary = Path(found)
    if not binary.exists():
        print(f"bench: binary not found: {args.binary}", file=sys.stderr)
        return 2
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    if args.rows > 0:
        size, rows, gen_ms = generate_rows(out, args.rows)
    else:
        target = int(args.gb * GB)
        size, rows, gen_ms = generate_bytes(out, target)

    commands = [
        ("schema", ["schema", str(out), "--json"]),
        ("filter", ["filter", str(out), "--where", "city=CDMX"]),
        ("search", ["search", str(out), "ZZZUNIQUE", "--limit", "10"]),
        ("quality", ["analyze", str(out), "--quality", "--json"]),
    ]
    results = []
    for name, command in commands:
        result = measure(str(binary), command)
        result["name"] = name
        results.append(result)
        print(f"{name}: {result['elapsedMs']} ms, rss={human_bytes(result['rssBytes'])}", file=sys.stderr)

    total_ms = round(sum(result["elapsedMs"] for result in results), 1)
    payload = {
        "generated": {
            "path": str(out),
            "bytes": size,
            "rows": rows,
            "generateMs": round(gen_ms * 1000, 1),
        },
        "binary": str(binary),
        "platform": f"{platform.system()}-{platform.machine()}",
        "commands": results,
        "totalMs": total_ms,
    }
    json_path = Path(args.json_out or (str(out) + ".bench.json"))
    md_path = Path(args.md_out or (str(out) + ".bench.md"))
    json_path.write_text(json.dumps(payload, indent=2) + "\n")
    summary = {
        "measuredAt": time.strftime("%Y-%m-%d"),
        "machine": f"{platform.system()}-{platform.machine()}",
        "binary": str(binary),
        "dataset": {
            "bytes": payload["generated"]["bytes"],
            "rows": payload["generated"]["rows"],
            "generateMs": payload["generated"]["generateMs"],
        },
        "commands": [
            {"name": item["name"], "elapsedMs": item["elapsedMs"], "rssBytes": item["rssBytes"], "exit": item["exit"]}
            for item in results
        ],
        "totalMs": total_ms,
        "notes": ["no page-cache drop", "not an SLA"],
    }
    summary_path = Path(str(out) + ".summary.json")
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    md = [
        f"# csvx benchmark: {human_bytes(size)} / {rows:,} rows",
        "",
        f"- binary: `{binary}`",
        f"- platform: `{payload['platform']}`",
        f"- generated: `{out}` in {payload['generated']['generateMs']} ms",
        "",
        "| command | elapsed | RSS |",
        "|---|---:|---:|",
    ]
    for result in results:
        md.append(f"| {result['name']} | {result['elapsedMs']} ms | {human_bytes(result['rssBytes'])} |")
    md += ["", f"Total measured: {total_ms} ms. Generated data is deterministic; no cache clearing is performed.", ""]
    md_path.write_text("\n".join(md))
    print(f"bench_json={json_path}")
    print(f"bench_md={md_path}")
    print(f"bench_summary={summary_path}")
    print(f"bench_bytes={size} bench_rows={rows} bench_total_ms={total_ms}")
    if args.clean:
        out.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
