#!/usr/bin/env python3
"""Smoke the Agent API routes against a running server."""
import argparse
import json
import socket
import subprocess
import tempfile
from urllib.parse import urlparse
import threading
import time
from pathlib import Path
import urllib.error
import urllib.request

def request(api, path, token=None, method="GET", body=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(api + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=20) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()

def sse_latency(api, sid, token):
    result = {}

    def reader():
        parsed = urlparse(api)
        try:
            with socket.create_connection((parsed.hostname, parsed.port), timeout=10) as sock:
                sock.sendall(
                    (
                        f"GET /agent/v1/sessions/{sid}/events?follow=1 HTTP/1.1\r\n"
                        f"Host: {parsed.hostname}\r\n"
                        f"Authorization: Bearer {token}\r\n"
                        "Connection: close\r\n\r\n"
                    ).encode()
                )
                while True:
                    chunk = sock.recv(1024)
                    if not chunk:
                        return
                    text = chunk.decode(errors="replace")
                    result["data"] = result.get("data", "") + text
                    if '"tool":"csvx_schema"' in text:
                        result["seen"] = time.monotonic()
                        return
        except Exception as error:  # noqa: BLE001
            result["error"] = str(error)

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    time.sleep(0.2)
    started = time.monotonic()
    status, body = request(
        api, f"/agent/v1/sessions/{sid}/actions", token, "POST",
        {"tool": "csvx_schema", "arguments": {"path": "orders.csv"}},
    )
    assert status == 200, body
    thread.join(timeout=5)
    if "seen" not in result:
        raise AssertionError(f"SSE did not deliver: {result}")
    return (result["seen"] - started) * 1000

def has_x(binary, path):
    run = subprocess.run(
        [binary, "view", path, "--json"], capture_output=True, text=True
    )
    assert run.returncode == 0, f"{run.stdout}\n{run.stderr}"
    rows = json.loads(run.stdout)["rows"]
    return any("X" in row for row in rows)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--api", default="http://127.0.0.1:8788")
    parser.add_argument("--file", default="orders.csv")
    parser.add_argument("--binary", default="target/release/csvx")
    parser.add_argument("--root", default=".")
    args = parser.parse_args()
    api = args.api.rstrip("/")
    status, body = request(api, "/agent/v1/manifest")
    assert status == 200, body
    manifest = json.loads(body)
    endpoint_names = ["actions", "audit", "replay", "curl", "runbook", "approvals"]
    for name in endpoint_names:
        assert name in manifest["endpoints"], f"manifest lacks {name}"
    status, body = request(api, "/agent/v1/sessions", method="POST", body={"agent": "smoke"})
    assert status == 201, body
    session = json.loads(body)
    token, sid = session["token"], session["sessionId"]
    routes = [
        f"/agent/v1/sessions/{sid}",
        f"/agent/v1/sessions/{sid}/actions",
        f"/agent/v1/sessions/{sid}/actions.md",
        f"/agent/v1/sessions/{sid}/actions.html",
        f"/agent/v1/sessions/{sid}/audit",
        f"/agent/v1/sessions/{sid}/trace",
        f"/agent/v1/sessions/{sid}/replay",
        f"/agent/v1/sessions/{sid}/curl",
        f"/agent/v1/sessions/{sid}/runbook",
        f"/agent/v1/sessions/{sid}/runbook.md",
        f"/agent/v1/sessions/{sid}/approvals",
        "/agent/v1/sessions",
    ]
    for route in routes:
        status, body = request(api, route, token)
        assert status == 200, f"{route}: {status} {body}"

    sse_ms = sse_latency(api, sid, token)
    assert sse_ms < 2000, f"SSE first frame took {sse_ms:.0f}ms"
    action = f"/agent/v1/sessions/{sid}/actions"
    source = Path(args.root, args.file)
    original = source.read_text()
    destructive = {
        "tool": "csvx_edit_apply",
        "arguments": {"path": args.file, "ops": "set:1:city:X"},
    }
    status, body = request(api, action, token, "POST", destructive)
    assert status == 428, f"expected 428 without approval, got {status}: {body}"
    status, body = request(
        api, f"/agent/v1/sessions/{sid}/approvals", token, "POST",
        {"tool": "csvx_edit_apply", "decision": "deny"},
    )
    assert status == 403, f"expected 403 on deny, got {status}: {body}"
    status, body = request(
        api, f"/agent/v1/sessions/{sid}/approvals", token, "POST",
        {"tool": "csvx_edit_apply", "decision": "approve"},
    )
    assert status == 200, body
    approval = json.loads(body)["approvalId"]
    destructive["approvalId"] = approval
    status, body = request(api, action, token, "POST", destructive)
    assert status == 200, body
    status, body = request(api, action, token)
    assert status == 200, body
    assert '"approval":"approved"' in body, body
    assert has_x(args.binary, str(source)), "approved action did not apply"
    actions_count = len(json.loads(body)["actions"])

    status, replay = request(api, f"/agent/v1/sessions/{sid}/replay", token)
    assert status == 200, replay
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as handle:
        handle.write(replay)
        replay_path = handle.name
    try:
        with tempfile.TemporaryDirectory() as replay_root:
            copy = Path(replay_root, args.file)
            copy.write_text(original)
            run = subprocess.run(
                [args.binary, "agent", "replay", "--root", replay_root, "--replay", replay_path],
                capture_output=True, text=True,
            )
            assert run.returncode == 0, f"{run.stdout}\n{run.stderr}"
            assert has_x(args.binary, str(copy)), "replay did not change the copy"
    finally:
        import os
        os.unlink(replay_path)

    status, body = request(api, action, token)
    assert status == 200, body
    after_count = len(json.loads(body)["actions"])
    assert after_count == actions_count, f"replay changed HTTP actions: {actions_count} -> {after_count}"

    print(
        f"routes={len(routes)} session={sid} actions={actions_count} "
        f"sse_ms={sse_ms:.0f} deny=403 approval=ok replay=ok"
    )

if __name__ == "__main__":
    main()
