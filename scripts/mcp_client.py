#!/usr/bin/env python3
"""Minimal MCP stdio client used by the end-to-end demo (no dependencies)."""
import argparse
import json
import os
import subprocess
import sys
import time

class Mcp:
    def __init__(self, binary, root):
        self.proc = subprocess.Popen(
            [binary, "mcp", "--root", root],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.seq = 0
        self.request("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, message):
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params=None):
        self.seq += 1
        self.send({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params or {}})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed: " + self.proc.stderr.read())
            message = json.loads(line)
            if "id" in message and message["id"] == self.seq:
                if "error" in message:
                    raise RuntimeError(message["error"])
                return message["result"]

    def call(self, name, arguments):
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise RuntimeError(text)
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return text

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--file", required=True)
    args = parser.parse_args()
    client = Mcp(args.binary, args.root)
    try:
        tools = client.request("tools/list")["tools"]
        print(f"tools={len(tools)}")
        schema = client.call("csvx_schema", {"path": args.file})
        print(f"schema_rows={schema['rows']}")
        filtered = client.call("csvx_filter", {"path": args.file, "where": "city=CDMX", "limit": 10})
        print(f"filter_matched={filtered['matched']}")
        gzip_path = os.path.join(args.root, "orders.csv.gz")
        if os.path.exists(gzip_path):
            gzip = client.call("csvx_search", {"path": "orders.csv.gz", "query": "CDMX", "materialize": True})
            print(f"gzip_matched={gzip.get('matched', 0)}")
            client.call("csvx_search", {"path": "orders.csv.gz", "query": "CDMX", "materialize": True})
            cache = client.call("csvx_cache_stats", {})
            print(f"cache_hits={cache.get('hits', 0)}")
            print(f"cache_entries={cache.get('entries', 0)}")
        plan_path = os.path.join(args.root, "plan.csv")
        if os.path.exists(plan_path):
            status = client.call("csvx_pipeline_status", {"path": "plan.csv", "verify": True, "contentCheck": True})
            print(f"status_fresh={sum(1 for step in status.get('steps', []) if step.get('fresh'))}")
        page = client.call("csvx_view", {"path": args.file, "start": 0, "count": 2})
        print(f"view_rows={len(page['rows'])}")
        edited = client.call("csvx_edit_set", {"path": args.file, "row": 1, "col": "city", "value": "DEMO"})
        print(f"edit_ok={'row' in edited or 'batch' in edited or isinstance(edited, dict)}")
        client.call("csvx_edit_undo", {"path": args.file})
        converted = client.call("csvx_convert", {"path": args.file, "format": "tsv", "out": "demo.tsv"})
        print(f"convert_ok={converted is not None}")
        job = client.call("csvx_job_submit", {"tool": "csvx_quality", "arguments": {"path": args.file, "key": "id", "group": "city"}})
        deadline = time.time() + 20
        while time.time() < deadline:
            status = client.call("csvx_job_status", {"id": job["job"]})
            if status["state"] != "running":
                print(f"job={status['state']}")
                break
            time.sleep(0.1)
        else:
            raise RuntimeError("job did not finish")
    finally:
        client.close()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
