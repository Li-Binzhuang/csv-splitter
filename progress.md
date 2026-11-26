# csvx 迭代进度

## 当前目标

继续 csvx 迭代：逐一校验 23 个 MCP 工具 schema 与 CLI 参数一致性；把 DSH 真实客户端验证做成可选脚本；继续加固/缩小 `crates/mcp`；保持 12 路由与 smoke 契约；每轮跑测试、全量 gate、AOCI、发布包，最多 push 一次。

## 已完成

- 删除 legacy MCP 协议循环，只保留官方 `rmcp`。
- `crates/mcp` 拆为 `lib.rs / json.rs / cache.rs / executor.rs / jobs.rs / rmcp_server.rs / tools.rs`，并新增 `executor/paths.rs`。
- 单文件行数上限进入 `check_layout.sh`。
- 官方 SDK 逐项调用全部 23 个工具，`sdk_calls=23`。
- DSH headless 作为真实 MCP 客户端验证成功。
- 修复 `csvx_edit_delete` 缺少 `count` 的真实 bug。
- `scripts/check_mcp_schema.py` 校验 23 工具、43 字段，`mcp_schema=ok`。
- `scripts/check_mcp_dsh.sh` 可选 DSH 回归，默认 `skip`。

## 当前进度

Round 5：`executor.rs` 468 → 78 行；23 工具命令分派拆到 `executor/commands.rs`（406 行）。
官方 SDK 仍 `sdk_calls=23`，gate 通过；发布包 SHA `911365bf…e534`。

Round 4：12 路由 smoke 增加无 token 检查（必须 401）与错误方法检查（必须 405）。
实测 `routes=12 route_bodies=12 unauth=12 method_guards=2`；发布包 SHA `5e3a52b4…67395`。

Round 3：12 路由 smoke 增加响应体契约：JSON / Markdown / HTML / JSONL replay / bash curl 脚本。
实测 `routes=12 route_bodies=12`；gate 通过；发布包 SHA `642a46fd…a9747c`。

Round 2：`executor.rs` 从 542 行降到 468 行，路径校验拆到 `executor/paths.rs`（80 行）；layout 覆盖子模块。

## 未完成

- 23 工具 schema 语义级参数一致性（当前是字段到 flag 的映射检查）。
- `executor/commands.rs` 406 行，后续可再按工具族拆分。
- 12 路由已有响应体、401、405 契约。
- 商业化/产品化未开始。

## 遇到的问题

- 上一轮 route smoke 曾把空 replay/curl 误判，已修正。
- 本轮 401/405 断言一次通过。

## 下一轮具体行动

1. AOCI 收尾并提交。
2. push 本轮提交一次。
3. 下一轮把 schema 检查升级为参数语义/必填校验。
