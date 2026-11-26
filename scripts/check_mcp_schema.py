#!/usr/bin/env python3
"""Check that every MCP tool schema field maps to a CLI flag it can pass."""
import json
import os
import re
import subprocess
import sys

BINARY = os.environ.get("CSVX_BIN") or ("target/debug/csvx" if os.path.exists("target/debug/csvx") else "target/release/csvx")
ROOT = os.environ.get("CSVX_ROOT", ".")
COMMAND = {
    "csvx_schema": "schema",
    "csvx_view": "view",
    "csvx_filter": "filter",
    "csvx_search": "search",
    "csvx_quality": "analyze",
    "csvx_pipeline_status": "pipeline",
    "csvx_edit_preview": "edit",
    "csvx_edit_apply": "edit",
    "csvx_edit_set": "edit",
    "csvx_edit_delete": "edit",
    "csvx_edit_undo": "edit",
    "csvx_edit_redo": "edit",
    "csvx_concat": "concat",
    "csvx_convert": "convert",
    "csvx_pipeline_run": "pipeline",
    "csvx_report": "report",
    "csvx_gate": "gate",
}
SERVER_ONLY = {"csvx_job_submit", "csvx_job_status", "csvx_job_cancel", "csvx_job_list", "csvx_cache_clear", "csvx_cache_stats"}
# Positional arguments or server-side meta, not --flags.
SKIP = {"path", "paths", "dir", "query", "tool", "arguments", "id", "wait"}
CAMEL = {
    "byName": "by-name", "ignoreHeader": "ignore-header", "contentCheck": "content-check",
    "minRows": "min-rows", "maxEmpty": "max-empty", "uniqueGlobal": "unique-global",
    "dryRun": "dry-run", "specText": "spec-text",
}

def send(proc, message):
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()

def read_response(proc, request_id):
    for line in proc.stdout:
        value = json.loads(line)
        if value.get("id") == request_id:
            return value
    raise RuntimeError("MCP closed before response")

proc = subprocess.Popen([BINARY, "mcp", "--root", ROOT], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
try:
    send(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}}})
    send(proc, {"jsonrpc": "2.0", "method": "notifications/initialized"})
    send(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    tools = read_response(proc, 2)["result"]["tools"]
finally:
    proc.stdin.close()
    proc.wait(timeout=10)

if len(tools) != 23:
    print(f"mcp_schema=fail tools={len(tools)}")
    sys.exit(1)

failures = []
checked = 0
shape_errors = []
allowed_types = {"string", "integer", "number", "boolean", "object", "array"}
for tool in tools:
    name = tool.get("name", "")
    schema = tool.get("inputSchema")
    if not isinstance(schema, dict) or schema.get("type") != "object":
        shape_errors.append(f"{name}: inputSchema is not an object schema")
        continue
    if schema.get("additionalProperties") is not False:
        shape_errors.append(f"{name}: additionalProperties is not false")
    properties = schema.get("properties")
    if not isinstance(properties, dict):
        shape_errors.append(f"{name}: properties is not an object")
        continue
    required = schema.get("required", [])
    if not isinstance(required, list) or not set(required).issubset(properties):
        shape_errors.append(f"{name}: required is not a subset of properties")
    for field, definition in properties.items():
        if not isinstance(definition, dict) or definition.get("type") not in allowed_types:
            shape_errors.append(f"{name}.{field}: invalid or missing type")
    if not isinstance(tool.get("annotations"), dict):
        shape_errors.append(f"{name}: missing annotations")

for tool in tools:
    name = tool["name"]
    if name in SERVER_ONLY:
        continue
    command = COMMAND.get(name)
    if command is None:
        failures.append(f"{name}: no CLI command mapping")
        continue
    help_text = subprocess.run([BINARY, command, "--help"], capture_output=True, text=True, check=False).stdout
    flags = set(re.findall(r"--[a-z][a-z-]*", help_text))
    for field in tool["inputSchema"].get("properties", {}):
        if field in SKIP:
            continue
        flag = "--" + CAMEL.get(field, field)
        checked += 1
        if flag not in flags:
            failures.append(f"{name}.{field}: {flag} absent from `{command} --help`")

print(f"mcp_schema_tools={len(tools)}")
print(f"mcp_schema_fields={checked}")
print(f"mcp_schema_shape_errors={len(shape_errors)}")
print(f"mcp_schema_failures={len(failures)}")
for failure in shape_errors:
    print(f"SHAPE {failure}")
for failure in failures:
    print(f"FAIL {failure}")
ok = not failures and not shape_errors
print("mcp_schema=" + ("ok" if ok else "fail"))
sys.exit(0 if ok else 1)
