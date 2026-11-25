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

## 内置 runner（OpenAI-compatible）

```bash
export CSVX_AGENT_API_KEY=...   # 不写入仓库
csvx agent run --root /path/to/data \
  --endpoint https://.../chat/completions \
  --model DeepSeek-V4.1-Flash-1017-Ark-Economy \
  --skill skills/csvx-data/SKILL.md \
  --prompt 'Use csvx_schema on t.csv, then reply with the row count.'
```

- 只从环境变量读取 key；curl 通过 stdin 收到 Authorization，key 不进 argv/磁盘；
- 模型每轮必须返回一个 JSON 动作：
  - `{"tool":"csvx_schema","arguments":{...}}`
  - `{"final":"..."}`
- 执行复用 `mcp_run_tool`；
- 默认不允许破坏性工具；`--allow-apply` 才放行；
- `--max-steps` 限制循环；
- `--skill` 把一份 Markdown skill 注入 system prompt；
- `--trace PATH` 记录模型回复、工具参数、观察值和 final；`--replay PATH` 不调模型，按 trace 重放工具调用（见 `docs/AGENT_TRACE.md`）；
- `--allow-apply` 才放行破坏性工具；实测 read→preview→apply 5 步 11.75 s，CSV 不动、journal 1 条。

实测 Ark Economy：

```text
无 skill：step 1 csvx_schema → step 2 csvx_view → 2，6.10 s
带 skill：step 1 csvx_schema → final 2，5.49 s
```

## HTTP Agent API（最小实现）

```bash
csvx agent serve --root /path/to/data --addr 127.0.0.1:8788
csvx agent manifest
```

基础路径 `/agent/v1`，JSON body：

| 方法 | 路径 | 作用 |
|---|---|---|
| GET | `/agent/v1/manifest` | 协议、工具、workflow、safety |
| POST | `/agent/v1/sessions` | 创建会话（root、agent 名、可选 allowedTools） |
| GET | `/agent/v1/sessions/{id}` | 状态/摘要 |
| POST | `/agent/v1/sessions/{id}/actions` | 执行一个 MCP 工具 |
| GET | `/agent/v1/sessions/{id}/events` | 审计事件回放；`?follow=1` 持续推送最多 60s |
| GET | `/agent/v1/sessions/{id}/audit` | 原始审计 JSONL；含 `tool`、`arguments`、`running/ok/error`、耗时 |
| GET | `/agent/v1/sessions/{id}/trace` | `audit` 的别名，便于和 runner 的 trace 概念对齐 |
| GET | `/agent/v1/sessions/{id}/replay` | 只导出最终 `ok/error` 动作，可直接喂给 `csvx agent replay` |
| GET | `/agent/v1/sessions/{id}/actions` | 最终动作清单 JSON（step/tool/arguments/status/approval/elapsedMs），含 unapproved 计数 |
| GET | `/agent/v1/sessions/{id}/actions.md` | 同一清单的 Markdown 表格，带步骤号并作为附件下载 |
| GET | `/agent/v1/sessions/{id}/actions.html` | 同一清单的 HTML 表格 |
| GET | `/agent/v1/sessions/{id}/curl` | 生成可复制的 curl 脚本；破坏性动作带 `__APPROVAL_ID__` 占位 |
| GET | `/agent/v1/sessions/{id}/approvals` | 当前未消费的 approvalId/tool 列表 |
| GET | `/agent/v1/sessions/{id}/runbook.md` | 一页式运行手册：待审批、动作、replay 命令；附件下载 |
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
- 创建时传 `allowedTools:["csvx_schema"]` 会把该会话限制在列出的工具，
  其他动作返回 **403**；不传则沿用默认全工具 + 破坏性审批；
- 审计写 `<root>/.csvx-agent/audit.jsonl`，不含 token/key；`/audit` 只返回
  当前 session 的行，不泄露同 root 的其他 session；每个动作有
  `running`、`approved`（审批时）和最终 `ok/error`，且包含工具参数（不含 token）；
- 所有路径 canonical + root 内；
- SSE 默认回放后关闭；`?follow=1` 轮询审计文件并持续推送最多 60s；
- 没有多用户角色，token 只是单用户本地能力边界。

## 和 DSH 的关系

- DSH 可直接把 csvx 作为 MCP server；
- 内置 runner 只使用本地 DSH 的 `DeepSeek-V4.1-Flash-1017-Ark-Economy`；
- 不修改用户 DSH profile，除非用户明确要求；
- 模型 key 不进仓库。

## 验证

- MCP initialize `instructions`、`csvx://agent` 已由 stdio 测试钉住；
- HTTP Agent API 已有端到端测试：创建会话 → schema → 428 审批 →
  approval → apply → 事件回放 → 审计 → close；
- `follow=1` 的持续推送仍由端到端连接读取首块验证。
