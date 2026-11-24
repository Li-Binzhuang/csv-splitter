#!/usr/bin/env python3
"""HTTP release smoke: /ops, /report chart+compare, /gate (local and global), /concat dir."""
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

BIN = os.environ.get("CSVX_BIN", os.path.join("target", "release", "csvx"))
errors = []

def check(name, ok, detail=""):
    print(("ok  " if ok else "FAIL") + " " + name + (("  " + detail) if detail else ""))
    if not ok:
        errors.append(name)

def request(base, path, method="GET"):
    req = urllib.request.Request(base + path, method=method)
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()

def main():
    root = tempfile.mkdtemp(prefix="csvx-smoke-")
    os.makedirs(os.path.join(root, "sub"))
    os.makedirs(os.path.join(root, "dup"))
    def write(name, text):
        with open(os.path.join(root, name), "w", encoding="utf-8") as file:
            file.write(text)
    write("t.csv", "id,city,amount\n1,A,10\n2,B,20\n")
    write("other.csv", "id,city,amount\n3,C,30\n")
    write("spec.csv", "title,Smoke\nsummary,\nchart,city:amount\ncompare,other.csv\n")
    write("sub/a.csv", "id,city\n1,A\n")
    write("sub/b.csv", "id,city\n2,B\n")
    write("dup/a.csv", "id\n1\n")
    write("dup/b.csv", "id\n1\n")

    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    base = f"http://127.0.0.1:{port}"
    process = subprocess.Popen(
        [BIN, "serve", "--root", root, "--addr", f"127.0.0.1:{port}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    try:
        ready = False
        for _ in range(100):
            try:
                status, _ = request(base, "/health")
                if status == 200:
                    ready = True
                    break
            except (urllib.error.URLError, ConnectionError):
                time.sleep(0.1)
        check("server ready", ready)
        status, body = request(base, "/")
        check("viewer html", status == 200 and "<h1>" in body, str(status))

        status, body = request(base, "/ops")
        check("ops 17", status == 200 and len(json.loads(body)["operations"]) == 17, str(status))

        status, body = request(base, "/report?path=t.csv&spec=spec.csv&format=html")
        check("report html", status == 200 and "<svg" in body and "Compare: other.csv" in body, str(status))

        status, body = request(base, "/gate?path=t.csv&minRows=1")
        check("gate file", status == 200 and json.loads(body)["passed"] is True, str(status))

        status, body = request(base, "/gate?path=dup&unique=id&uniqueGlobal=1")
        report = json.loads(body)
        check("gate global", status == 200 and report["passed"] is False and report["uniqueGlobal"]["passed"] is False, str(status))

        status, body = request(base, "/concat?dir=sub&out=all.csv", "POST")
        job = json.loads(body)["job"] if status == 202 else ""
        for _ in range(100):
            status, body = request(base, f"/job?id={job}")
            state = json.loads(body).get("state")
            if state != "running":
                break
            time.sleep(0.1)
        check("concat dir", state == "done" and os.path.getsize(os.path.join(root, "all.csv")) > 0, str(state))
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
    print("errors:", errors)
    return 1 if errors else 0

if __name__ == "__main__":
    sys.exit(main())
