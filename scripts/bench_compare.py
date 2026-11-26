#!/usr/bin/env python3
"""Compare two benchmark summaries and flag elapsed-time regressions."""
import argparse
import json
import sys


def load(path):
    data = json.load(open(path, encoding="utf-8"))
    commands = data.get("commands")
    if not isinstance(commands, list) or not data.get("totalMs"):
        raise ValueError(f"{path}: missing commands/totalMs")
    return data


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline")
    parser.add_argument("candidate")
    parser.add_argument("--max-regression-percent", type=float, default=20.0)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    try:
        baseline = load(args.baseline)
        candidate = load(args.candidate)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"bench_compare=fail {error}", file=sys.stderr)
        return 2

    base = {item["name"]: item for item in baseline["commands"]}
    cand = {item["name"]: item for item in candidate["commands"]}
    missing = sorted(set(base) - set(cand))
    added = sorted(set(cand) - set(base))
    failed = [{"name": name, "exit": cand[name].get("exit")} for name in cand if cand[name].get("exit") not in (0, None)]
    regressions = []
    for name in sorted(set(base) & set(cand)):
        before = base[name].get("elapsedMs")
        after = cand[name].get("elapsedMs")
        if not before:
            raise SystemExit(f"bench_compare=fail baseline has no elapsedMs for {name}")
        pct = (after - before) / before * 100.0
        if pct > args.max_regression_percent:
            regressions.append({"name": name, "baselineMs": before, "candidateMs": after, "deltaPercent": round(pct, 2)})

    total_pct = (candidate["totalMs"] - baseline["totalMs"]) / baseline["totalMs"] * 100.0
    total_regression = total_pct > args.max_regression_percent
    dataset_mismatch = []
    base_dataset = baseline.get("dataset")
    cand_dataset = candidate.get("dataset")
    if isinstance(base_dataset, dict) and isinstance(cand_dataset, dict):
        for key in ("bytes", "rows"):
            if key in base_dataset and key in cand_dataset and base_dataset[key] != cand_dataset[key]:
                dataset_mismatch.append({"key": key, "baseline": base_dataset[key], "candidate": cand_dataset[key]})
    result = {
        "ok": not missing and not regressions and not total_regression and not dataset_mismatch and not failed,
        "baseline": args.baseline,
        "candidate": args.candidate,
        "maxRegressionPercent": args.max_regression_percent,
        "totalDeltaPercent": round(total_pct, 2),
        "totalRegression": total_regression,
        "missingCommands": missing,
        "addedCommands": added,
        "datasetMismatch": dataset_mismatch,
        "failedCommands": failed,
        "regressions": regressions,
    }
    if args.json:
        print(json.dumps(result, indent=2, ensure_ascii=False))
    else:
        print(f"bench_compare={'ok' if result['ok'] else 'fail'} total_delta={result['totalDeltaPercent']}%")
        for item in regressions:
            print(f"regression {item['name']}: {item['baselineMs']}->{item['candidateMs']}ms ({item['deltaPercent']}%)")
        for name in missing:
            print(f"missing {name}")
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
