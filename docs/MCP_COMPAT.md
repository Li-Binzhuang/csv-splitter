# MCP 客户端兼容记录

`csvx mcp` 只有一个传输：官方 `rmcp`。手写 JSON-RPC 循环已删除。
以下两条路径都在本机实际跑过，不是接口说明。

## 1. 官方 MCP SDK（脚本化，进 gate）

```bash
bash scripts/check_mcp_sdk.sh
```

脚本用 `scripts/mcp_sdk/client.mjs`（`@modelcontextprotocol/sdk` +
`StdioClientTransport`）启动二进制，逐项调用全部 23 个工具，并断言：

```text
sdk_tools=23
sdk_names=23
sdk_calls=23
progress_events>=1
structured_ok=true
pipeline_progress_events>=1
status_fresh>=1
gzip_matched=1
cache_hits>=1
cache_after=0
job=done
job_listed=true
cancel_state=done
job_reject_code=job_start_error
unknown_code=unknown_tool
required_rejections=20
enum_rejections=2
range_rejections=8
capabilities_tools=true
capabilities_resources=true
instructions_chars=529
resources=2
resource_protocol=csvx-agent/1
resource_tools=23
resource_endpoints=8
resource_workflow=6
resource_root_bound=true
resource_operations=17
resource_error_codes=10
unknown_resource_error=true
```

gate 的 `mcp-sdk` 阶段默认使用 `target/debug/csvx`，避免测到旧的 release 产物；
node 或 `scripts/mcp_sdk/node_modules` 不存在时明确跳过。

## 2. DSH 作为真实 MCP 客户端

用临时 patch 把 csvx 注册进一次性的 DSH headless 运行，不改用户 profile：

```yaml
# /tmp/csvx-mcp-patch.yml
- insert:
    - id: csvx-mcp
      name: "@deepseek-ai/dsh-mcp-client"
      config:
        serverName: csvx
        transport: stdio
        command: "/path/to/csvx"
        args: ["mcp", "--root", "/tmp/csvx-dsh-test"]
        failOnStartupError: true
```

```bash
dsh headless --patch /tmp/csvx-mcp-patch.yml \
  --permission-mode read-only \
  "Use the csvx MCP server to call its csvx_schema tool on \
   /tmp/csvx-dsh-test/orders.csv and report the row count. Do not edit any file."
```

实测回答：

```text
`/tmp/csvx-dsh-test/orders.csv` has 2 rows.

- Columns: id (number), city (text)
- No empty cells in either column
```

说明 DSH 的 MCP 客户端完成握手、`tools/list` 与 `tools/call`，并读到 csvx 的结果。

## 3. Schema 与 CLI 参数一致性

```bash
python3 scripts/check_mcp_schema.py
```

实测：

```text
mcp_schema_tools=23
mcp_schema_fields=43
mcp_schema_failures=0
mcp_schema=ok
```

检查每个 MCP tool 的非位置参数都能在对应 CLI 命令的 `--help` 中找到。
这项已进 gate 的 `mcp-schema` 阶段。

## 4. 可重复的 DSH 检查（默认跳过）

```bash
CSVX_DSH_TEST=1 bash scripts/check_mcp_dsh.sh
```

只有显式设置 `CSVX_DSH_TEST=1` 才会启动 DSH headless 模型运行；
未设置时输出 `mcp_dsh=skip`，不消耗模型。2026-11-26 实跑一次：

```text
mcp_dsh=ok
DSH: `csvx_schema` reports 2 rows (id, city); no edits.
```

## 契约

- 23 个工具名与 schema 由 `crates/mcp/src/tools.rs` 冻结；
- 每次改动都跑 `bash scripts/gate.sh`（含 SDK 阶段）；
- legacy 不再保留回滚入口；回滚靠 git 历史。
