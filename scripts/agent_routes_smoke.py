#!/usr/bin/env python3
"""Smoke the Agent API routes against a running server."""
import argparse
import json
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

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--api", default="http://127.0.0.1:8788")
    parser.add_argument("--file", default="orders.csv")
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

    action = f"/agent/v1/sessions/{sid}/actions"
    destructive = {
        "tool": "csvx_edit_set",
        "arguments": {"path": args.file, "row": 1, "col": "city", "value": "X"},
    }
    status, body = request(api, action, token, "POST", destructive)
    assert status == 428, f"expected 428 without approval, got {status}: {body}"
    status, body = request(
        api, f"/agent/v1/sessions/{sid}/approvals", token, "POST",
        {"tool": "csvx_edit_set", "decision": "approve"},
    )
    assert status == 200, body
    approval = json.loads(body)["approvalId"]
    destructive["approvalId"] = approval
    status, body = request(api, action, token, "POST", destructive)
    assert status == 200, body
    status, body = request(api, action, token)
    assert status == 200, body
    assert '"approval":"approved"' in body, body

    print(f"routes={len(routes)} session={sid} approval=ok")

if __name__ == "__main__":
    main()
