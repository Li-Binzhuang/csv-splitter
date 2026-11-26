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

Round 2：`executor.rs` 从 542 行降到 468 行，路径校验拆到 `executor/paths.rs`（80 行）；layout 覆盖子模块。MCP 单测 7、集成 7、全量 gate 通过；发布包已验证。

## 未完成

- 23 工具 schema 语义级参数一致性（当前是字段到 flag 的映射检查）。
- `executor.rs` 仍可继续拆命令分派。
- 12 路由只有 smoke，没有逐路由契约矩阵。
- 商业化/产品化未开始。

## 遇到的问题

- 上一轮 push 一次失败：`Please make sure you have the correct access rights`。按规则未重试，`df2c10d8` 仍在本地领先。
- 本轮 gate/package 已通过，AOCI 收尾后尝试 push 一次。

## 下一轮具体行动

1. 跑 MCP 测试、全量 gate、打包、AOCI。
2. push 本轮提交一次。
3. 若 push 仍失败，继续本地提交并记录，不阻塞代码工作。
4. 继续拆 `executor.rs` 的命令分派或补 12 路由契约矩阵。
