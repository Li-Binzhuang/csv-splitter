#!/usr/bin/env python3
"""Measure two identical MCP search calls; the second should hit the materialize cache."""
import argparse, json, os, resource, subprocess, sys, time

def call(proc, state, name, arguments):
    state["seq"] += 1
    proc.stdin.write(json.dumps({"jsonrpc":"2.0","id":state["seq"],"method":"tools/call","params":{"name":name,"arguments":arguments}})+"\n")
    proc.stdin.flush()
    while True:
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError(proc.stderr.read())
        msg = json.loads(line)
        if msg.get("id") == state["seq"]:
            result = msg["result"]
            if result.get("isError"):
                raise RuntimeError(result["content"][0]["text"])
            return json.loads(result["content"][0]["text"])

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--file", required=True, help="path relative to --root")
    parser.add_argument("--query", default="ZZZ")
    args = parser.parse_args()
    env = dict(os.environ, CSVX_MCP_CACHE_BYTES=str(8 * 1024**3))
    proc = subprocess.Popen([args.binary, "mcp", "--root", args.root], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, env=env)
    state = {"seq": 0}
    try:
        proc.stdin.write(json.dumps({"jsonrpc":"2.0","method":"notifications/initialized"})+"\n")
        proc.stdin.flush()
        state["seq"] += 1
        proc.stdin.write(json.dumps({"jsonrpc":"2.0","id":state["seq"],"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}})+"\n")
        proc.stdin.flush()
        while True:
            line = proc.stdout.readline()
            if not line: raise RuntimeError(proc.stderr.read())
            msg = json.loads(line)
            if msg.get("id") == state["seq"]: break
        for run in (1, 2):
            started = time.perf_counter()
            data = call(proc, state, "csvx_search", {"path": args.file, "query": args.query, "materialize": True})
            print(f"run{run}_s={time.perf_counter()-started:.3f} scanned={data.get('scanned')} matched={data.get('matched')}")
    finally:
        proc.stdin.close(); proc.wait(timeout=10)
    raw = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    mb = raw / 1024 / 1024 if sys.platform == "darwin" else raw / 1024
    print(f"maxrss_mb={mb:.0f}")

if __name__ == "__main__":
    main()
