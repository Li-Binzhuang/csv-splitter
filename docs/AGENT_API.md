# Agent API / 原生 Agent 接入

状态：**MCP 原生接入已可用；HTTP Agent API 尚未实现**。仍无前端、无多用户鉴权。

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

## 计划中的 HTTP Agent API（未实现）

基础路径 `/agent/v1`，JSON body：

| 方法 | 路径 | 作用 |
|---|---|---|
| GET | `/agent/v1/manifest` | 协议、工具、限制 |
| POST | `/agent/v1/sessions` | 创建会话（root、agent 名、允许的动作） |
| GET | `/agent/v1/sessions/{id}` | 状态/摘要 |
| POST | `/agent/v1/sessions/{id}/actions` | 执行一个工具/操作 |
| GET | `/agent/v1/sessions/{id}/events` | SSE 事件 |
| POST | `/agent/v1/sessions/{id}/approvals` | 批准/拒绝破坏性动作 |
| POST | `/agent/v1/sessions/{id}/close` | 关闭并落审计 |
| GET | `/agent/v1/audit?session=` | 审计记录 |

约束：

- 会话 token 只在创建响应返回一次；
- token 绑定 root、允许的 tools、过期时间；
- 审计写 `.csvx-agent/audit.jsonl`，不含 token/key；
- apply 必须带审批；
- 所有路径 canonical + root 内。

## 和 DSH 的关系

- DSH 可直接把 csvx 作为 MCP server；
- 内置 runner 只使用本地 DSH 的 `DeepSeek-V4.1-Flash-1017-Ark-Economy`；
- 不修改用户 DSH profile，除非用户明确要求；
- 模型 key 不进仓库。

## 验证

- MCP initialize `instructions`、`csvx://agent` 已由 stdio 测试钉住；
- HTTP Agent API 尚未进入测试，计划单列，避免把“计划”写成“已完成”。
