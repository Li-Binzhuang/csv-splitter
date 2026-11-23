#!/usr/bin/env python3
"""Generate a reproducible pilot report for one CSV sample (local only)."""
import argparse
import json
import os
import platform
import resource
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone


def run(binary, args, timeout):
    started = time.perf_counter()
    proc = subprocess.run(
        [binary, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    elapsed = time.perf_counter() - started
    return proc, elapsed


def rss_mb():
    value = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    if sys.platform == "darwin":
        return round(value / (1024 * 1024), 1)
    return round(value / 1024, 1)


def parse_json(text):
    text = text.strip()
    if not text:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--binary", default=os.environ.get("CSVX_PILOT_BINARY", "target/release/csvx")
    )
    parser.add_argument(
        "--file", default=os.environ.get("CSVX_PILOT_FILE", "")
    )
    parser.add_argument(
        "--where", default=os.environ.get("CSVX_PILOT_WHERE", "")
    )
    parser.add_argument("--key", default=os.environ.get("CSVX_PILOT_KEY", ""))
    parser.add_argument("--group", default=os.environ.get("CSVX_PILOT_GROUP", ""))
    parser.add_argument("--out", default=os.environ.get("CSVX_PILOT_OUT", "pilot-report.md"))
    parser.add_argument("--timeout", type=int, default=3600)
    parser.add_argument(
        "--skip-export",
        action="store_true",
        default=bool(os.environ.get("CSVX_PILOT_SKIP_EXPORT")),
    )
    parser.add_argument(
        "--skip-quality",
        action="store_true",
        default=bool(os.environ.get("CSVX_PILOT_SKIP_QUALITY")),
    )
    args = parser.parse_args()
    if not args.file:
        sys.exit("set CSVX_PILOT_FILE (or the file option)")

    source = os.path.abspath(args.file)
    size = os.path.getsize(source)
    schema, schema_s = run(args.binary, ["schema", source, "--json"], args.timeout)
    if schema.returncode != 0:
        sys.exit(f"schema failed: {schema.stderr.strip()}")
    meta = parse_json(schema.stdout) or {}

    rows = []
    if args.where:
        filtered, filtered_s = run(
            args.binary, ["filter", source, "--where", args.where, "--json"], args.timeout
        )
        if filtered.returncode == 0:
            payload = parse_json(filtered.stdout) or {}
            rows.append(("filter（扫描）", filtered_s, payload.get("matched", "?")))
        if not args.skip_export and filtered.returncode == 0:
            with tempfile.TemporaryDirectory() as tmp:
                output = os.path.join(tmp, "out.csv")
                exported, exported_s = run(
                    args.binary,
                    ["filter", source, "--where", args.where, "-o", output],
                    args.timeout,
                )
                result = (
                    f"{os.path.getsize(output)} bytes" if os.path.exists(output) else "失败"
                )
                rows.append(("filter（物化导出）", exported_s, result))

    search, search_s = run(
        args.binary,
        ["search", source, "__csvx_pilot_probe__", "--json", "--limit", "1"],
        args.timeout,
    )
    rows.append(("search（0 命中，全扫）", search_s, "全文件"))

    if not args.skip_quality:
        quality_args = ["analyze", source, "--quality", "--json"]
        if args.key:
            quality_args += ["--key", args.key]
        if args.group:
            quality_args += ["--group", args.group]
        quality, quality_s = run(args.binary, quality_args, args.timeout)
        rows.append(
            ("quality", quality_s, "ok" if quality.returncode == 0 else "失败")
        )

    version, _ = run(args.binary, ["--version"], args.timeout)
    version_text = version.stdout.strip() if version.returncode == 0 else "?"
    out = [
        "# csvx 试点报告",
        "",
        f"- 生成时间：{datetime.now(timezone.utc).isoformat()}",
        f"- 机器：{platform.platform()} / {platform.machine()}",
        f"- 版本：`{version_text}`",
        f"- 样本：`{os.path.basename(source)}`（{size} bytes）",
        f"- 行/列：{meta.get('rows', '?')} / {meta.get('count', '?')}",
        f"- 过滤条件：`{args.where or '(未指定)'}`",
        "",
        "| 操作 | 墙钟秒 | 结果 |",
        "|---|---:|---|",
    ]
    for name, elapsed, result in rows:
        out.append(f"| {name} | {elapsed:.2f} | {result} |")
    out += [
        "",
        "## 说明",
        "",
        "- 以上是单次本地实测，不是 SLA 或承诺；未做 drop cache。",
        "- 压缩输入、重复块或合成数据会改变结果，报告需与样本一起解读。",
        "- 子进程峰值 RSS（近似，所有子进程取最大）："
        f"{rss_mb()} MB；csvx 自身峰值见 BENCHMARKS。",
        "- 本报告不含数据内容、遥测或网络访问。",
        "",
    ]
    text = "\n".join(out)
    with open(args.out, "w", encoding="utf-8") as handle:
        handle.write(text)
    print(f"wrote {args.out}")
    print(text)


if __name__ == "__main__":
    main()
