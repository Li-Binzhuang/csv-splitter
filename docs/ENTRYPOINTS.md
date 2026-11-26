# 入口层规则 / Entrypoints

这份文档约束 **入口层**：CLI、MCP、Agent API、HTTP API。`core` 的分层见
[`ARCHITECTURE.md`](ARCHITECTURE.md)。规则由 `scripts/check_layout.sh` 检查并接入 gate。

## 为什么需要它

当前 `core` 是 41 个职责明确的模块，但入口层只有三个大文件：

| 文件 | 行数 | 负责 |
|---|---:|---|
| `crates/cli/src/main.rs` | 13,266 | 所有 CLI 命令、参数、输出、报表 |
| `crates/cli/src/serve.rs` | 9,651 | HTTP 端点 + MCP 工具注册/执行器 |
| `crates/cli/src/agent.rs` | 1,652 | HTTP Agent API、session、审批、审计、模型循环 |

结果是：改一个 MCP 工具会碰到 HTTP 端点，改一个 CLI 命令会碰到报表和 watch；
任何新功能都没有明确的落点。

## 目标目录

```text
crates/
  core/       数据引擎；唯一业务真相
  mcp/        已存在：MCP stdio 协议、工具 schema、执行器（薄适配层）
  agent/      已存在：Agent API；axum 传输 + session/审批/审计/trace/replay
  api/        axum HTTP API（可选，待抽）
  cli/        参数解析与命令分发；HTTP 端点暂留 serve.rs
docs/
  ARCHITECTURE.md
  ENTRYPOINTS.md
scripts/
  check_layout.sh
```

依赖方向只能是：

```text
cli / mcp / api / agent  →  core
core  →  不允许依赖任何入口
```

## 目录纪律

- 根目录只放项目元数据、`crates/`、`docs/`、`examples/`、`scripts/`、`skills/`；
  不新增根目录 `.rs` 或临时脚本。
- 源码只在 `crates/<crate>/src`；测试在 `crates/<crate>/tests`。
- `crates/cli/src` 当前只允许 `main.rs`、`serve.rs`、`cli_support.rs` 和 `commands/`；命令模块逐条
  登记在 `check_layout.sh`，新增前先更新本文件，不允许随手加一个 `foo.rs`。
- 入口文件目标 ≤300 行。当前超标文件是迁移对象，只降不升；`main.rs` 不得继续增长。
- 一个命令一个模块：`commands/split.rs`、`commands/report.rs`、`commands/xlsx.rs`。
- MCP 工具定义与执行器独立；CLI 不得直接调用内部 HTTP 函数。

## 与 DSH 的分工

DSH 是 agent 宿主（计划、工具调用、会话、审批展示）；csvx 只提供数据工具。
长期接法是：

```text
DSH ──MCP stdio──► csvx mcp ──► csvx-core
```

因此：

- 不把 DSH（Node/TypeScript）嵌进 Rust 二进制；
- 不继续扩写内置 runner；它只保留为兼容/离线选项；
- MCP 工具名、参数 schema、错误码是稳定契约，迁移期间不得改名。

## 冻结契约（迁移期间）

- CLI 命令名、帮助表、错误码；
- MCP 23 个工具名与 `inputSchema`（`csvx mcp`）；
- Agent API 的 session/approval/audit/trace/replay/events 路由语义；
- 发布包成员、Agent 路由 smoke 的 `routes=12`、`replay=actions`、`sse_ms<100`。

任何一步迁移都必须在同一轮保持：

```text
cargo test --workspace
cargo test -p csvx-core --features sql
bash scripts/gate.sh          # 含官方 MCP SDK 端到端检查
bash scripts/check_mcp_sdk.sh # 单独复跑 23 工具/progress/job
python3 scripts/check_mcp_schema.py # schema 与 CLI flags 一致性
CSVX_DSH_TEST=1 bash scripts/check_mcp_dsh.sh # 可选真实 DSH 回归
AOCI maintain
bash scripts/package_release.sh
```

## 迁移顺序

1. **冻结 + 目录制度**（本文件、`check_layout.sh`、gate）。
2. **抽 `crates/mcp`（已完成）**：官方 `rmcp` 是默认 stdio 传输；手写协议循环已删除。
   模块拆分：`tools.rs`（23 工具 schema）、`jobs.rs`（job 状态）、`cache.rs`（材料化缓存）、
   `executor.rs`（工具执行/命令构建）、`executor/paths.rs`（路径校验）、`executor/commands.rs`（工具分派）、`executor/commands/{args,concat}.rs`、`json.rs`（McpJson/McpParser）、
   `rmcp_server.rs`（传输）、`rmcp_server/jobs.rs`（job/cache 工具）、`rmcp_server/transport.rs`（stdin/stdout 泵与 legacy shim）、`rmcp_server/audit.rs`（调用审计）、`tests.rs`（单元测试）、`lib.rs`（入口/manifest）。HTTP 端点仍在 `serve.rs`。
   单文件上限由 `check_layout.sh` 强制：`pipeline.rs` 2300、`pipeline_history.rs` 250、`mcp_config.rs` 120；
   MCP：`lib.rs` 450、`executor.rs` 700、
   `rmcp_server.rs` 500、`jobs.rs` 400、`cache.rs`/`json.rs` 350。
3. **抽 `crates/agent`（已完成传输替换）**：控制面在 `crates/agent`；HTTP/SSE
   由 axum 承载，session/审批/审计/trace/replay 语义保持不变。
4. **拆 `main.rs`（已完成）**：`agent`/`analyze`/`book`/`columns`/`dedupe`/`ops`/`schema`/`verify`/`cache`/`chart`/`concat`/`convert`/`demo`/`edit`/`extract`/`files`/`filter`/`format`/`gate`/`groupby`/`index`/`layout`/`lookup`/`mcp`/`merge`/`partition`/`pipeline`/`pivot`/`preview`/`repair`/`report`/`search`/`serve`/`shared`/`sort`/`split`/`sql`/`stats`/`validate`/`view` 已迁到 `commands/`；
   其余命令按组迁移，CLI 只保留 dispatch；最后抽 `serve` 剩余 HTTP。

每一步只做一层，不同时重写 core 行为。

> 注：`main.rs` 已完成拆分，命令在 `commands/`（含 `pipeline_history.rs`），helper 在 `cli_support.rs`。

## 客户端兼容

官方 SDK 与 DSH 真实客户端验证记录见 `docs/MCP_COMPAT.md`；SDK 检查已进 gate。
