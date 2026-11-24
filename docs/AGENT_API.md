# Agent API / 原生 Agent 接入

状态：**MCP 原生接入与 HTTP Agent API 最小可用；SSE 目前是事件回放，不是长连接实时推送**。仍无前端、无多用户鉴权。

## 已经可用：MCP

任何支持 MCP 的 Agent 都可以直接挂载 csvx：

```json
{
  "mcpServers": {
    "csvx": {
      "command": "/path/to/csvx",
      "args": ["mcp", "--root", "/path/to/data"]
    }
  }
}
```

`initialize` 现在返回一段 `instructions`，告诉 Agent：

1. 先 `csvx_schema`、`csvx_view`；
2. 编辑先 `csvx_edit_preview`，再 `csvx_edit_apply`；
3. 所有路径必须在 root 内；
4. 长任务用 `csvx_job_submit`；
5. 报表/门禁/xlsx 用对应工具；
6. 没有 GUI、没有多用户鉴权，破坏性动作必须显式。

工具发现仍用标准 `tools/list`；另外新增资源：

```text
csvx://agent
```

返回：

- protocol `csvx-agent/1`
- transport `mcp-stdio`
- tools 清单（23 个）
- workflow：schema → view → preview → apply
- safety：preview_before_apply、root_bound、single_user、gui=false
- instructions 同一段话

## HTTP Agent API（最小实现）

```bash
csvx agent serve --root /path/to/data --addr 127.0.0.1:8788
csvx agent manifest
```

基础路径 `/agent/v1`，JSON body：

| 方法 | 路径 | 作用 |
|---|---|---|
| GET | `/agent/v1/manifest` | 协议、工具、workflow、safety |
| POST | `/agent/v1/sessions` | 创建会话（root、agent 名） |
| GET | `/agent/v1/sessions/{id}` | 状态/摘要 |
| POST | `/agent/v1/sessions/{id}/actions` | 执行一个 MCP 工具 |
| GET | `/agent/v1/sessions/{id}/events` | 审计事件回放（SSE 格式，读完即关） |
| POST | `/agent/v1/sessions/{id}/approvals` | 批准/拒绝破坏性工具 |
| POST | `/agent/v1/sessions/{id}/close` | 关闭并落审计 |

动作：

```json
{
  "tool": "csvx_edit_apply",
  "arguments": {"path": "t.csv", "ops": "set:1:qty:9"},
  "approvalId": "appr-..."
}
```

破坏性工具（edit apply/set/delete/undo/redo、concat、convert、job submit、
pipeline run、job cancel、cache clear）没有 `approvalId` 时返回 **428**。

约束：

- 会话 token 只在创建响应返回一次，绑定 root 和 8 小时 TTL；
- 审计写 `<root>/.csvx-agent/audit.jsonl`，不含 token/key；
- 所有路径 canonical + root 内；
- SSE 当前只回放审计事件，不是实时长连接；
- 没有多用户角色，token 只是单用户本地能力边界。

## 和 DSH 的关系

- DSH 可直接把 csvx 作为 MCP server；
- 内置 runner 只使用本地 DSH 的 `DeepSeek-V4.1-Flash-1017-Ark-Economy`；
- 不修改用户 DSH profile，除非用户明确要求；
- 模型 key 不进仓库。

## 验证

- MCP initialize `instructions`、`csvx://agent` 已由 stdio 测试钉住；
- HTTP Agent API 已有端到端测试：创建会话 → schema → 428 审批 →
  approval → apply → 事件回放 → 审计 → close。
