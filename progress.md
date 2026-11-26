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

Round 1（审计目标）：MCP `call_tool` 落盘审计已实现，默认 `.csvx/mcp-audit.jsonl`；
参数仅记录 key/type/长度，4 MiB 轮转；`csvx://audit` 可读；新增 3 个单测，
MCP 单测 11；SDK `resources=3 audit_entries=63 audit_redacted=true`；发布包 SHA `ecdd3286…e15fd`。

Round 26：MCP 单测从 7 增至 8，新增命令映射拒绝非法 count 与字符串 force；发布包 SHA `11932a3f…24951`。

Round 25：SDK 校验 annotations 语义：只读 12、破坏性 10、openWorld 1；发布包 SHA `b09025d8…82298d`。

Round 24：schema 结构校验：23 个 tool 全部 object/additionalProperties=false/
required 子集/字段类型/annotations；`mcp_schema_shape_errors=0`；发布包 SHA `5d476709…9767d`。

Round 23：布尔参数类型严格化，`force`/`dryRun` 传字符串被拒绝：
`bool_rejections=2`；发布包 SHA `c7042030…587dfc`。

Round 22：SDK 增加 root escape 拒绝 `root_escape_rejections=2`；HTTP smoke 增加
错误 token 401 与未知路由 404；发布包 SHA `77460652…d5d3`。

Round 21：push 重试成功；SDK 增加 xlsx 转换与 HTML report 覆盖：
`xlsx_ok=true report_html=true`；发布包 SHA `7d922c57…2fc67`。

Round 20：`executor/commands.rs` 360 → 259 行；参数助手与 concat 拆到
`executor/commands/{args,concat}.rs`；push 一次仍失败（send-pack 断连）；发布包 SHA `7730c4b1…2fba`。

Round 19：`rmcp_server.rs` 319 → 207 行；stdin/stdout 泵与 legacy initialize shim
拆到 `rmcp_server/transport.rs`（116 行）；发布包 SHA `765848bf…b1d60`。

Round 18：`rmcp_server.rs` 410 → 319 行，job/cache 工具处理拆到 `rmcp_server/jobs.rs`（98 行）；发布包 SHA `1f71b4fa…03fe`。

Round 17：`crates/mcp/src/lib.rs` 312 → 55 行，7 个单元测试整体迁到 `tests.rs`；
layout 覆盖 tests.rs（≤350）；发布包 SHA `8a05a259…7701a`。

Round 16：HTTP/MCP manifest 契约：protocol、8 endpoints、6 workflow、root_bound；
发布包 SHA `c007988d…193d3c`。

Round 15：12 路由全部做错误方法检查（POST 到 GET-only、DELETE 到 POST 路由），
`method_guards=12`；发布包 SHA `5b207658…84be3`。

Round 14：push 重试成功；`executor/commands.rs` 430 → 360 行（11 处 Bool 解析收进 `flag_or`）；
行数上限收紧到 450；发布包 SHA `74ebe5d8…4161e`。

Round 13：实跑可选 DSH 回归 `CSVX_DSH_TEST=1 bash scripts/check_mcp_dsh.sh`，
DSH headless 经 MCP 调用 `csvx_schema` 返回 2 行；`mcp_dsh=ok`；发布包 SHA `26bb5463…f3daf`。

Round 12：SDK 验证 initialize 能力与指令、未知资源错误：tools/resources=true、
instructions 529 字符、unknown_resource_error=true；发布包 SHA `0ea6df47…a7d60`。

Round 11：官方 SDK 同时验证 MCP resources：2 个资源、`csvx://agent` 23 工具、
`csvx://operations` 17 操作与 10 错误码；发布包 SHA `c95c7fab…254`。

Round 10：12 路由 smoke 增加 `events`、`close`、关闭后 404 生命周期；
实测 `events=200 close=200 after_close=404`，发布包 SHA `93a19f85…8aee9`。

Round 9：`csvx_job_status wait=31` 拒绝（wait_range）；`csvx_gate minRows=-1` 拒绝；`range_rejections=8`；发布包 SHA `d3a0c69f…782d1`。

Round 8：MCP 层按 schema 上界拒绝 `view count=1001`、`filter/search limit=1001`、`pipeline jobs=65`；
`range_rejections=6`。

Round 7：SDK 增加枚举/范围语义：`convert`/`report` 非法 format、`view count=0`、`edit_set row=0` 均拒绝。
实测 `enum_rejections=2 range_rejections=2`；发布包 SHA `84f231e8…779ac`；`csvx_report` 不再把未知 format 静默成 md。

Round 6：SDK 对 20 个有必填参数的工具传空参数，全部返回错误；`required_rejections=20`；发布包 SHA `abcfc0f5…71cbc`。

Round 5：`executor.rs` 468 → 78 行；23 工具命令分派拆到 `executor/commands.rs`（406 行）。
官方 SDK 仍 `sdk_calls=23`，gate 通过；发布包 SHA `911365bf…e534`。

Round 4：12 路由 smoke 增加无 token 检查（必须 401）与错误方法检查（必须 405）。
实测 `routes=12 route_bodies=12 unauth=12 method_guards=2`；发布包 SHA `5e3a52b4…67395`。

Round 3：12 路由 smoke 增加响应体契约：JSON / Markdown / HTML / JSONL replay / bash curl 脚本。
实测 `routes=12 route_bodies=12`；gate 通过；发布包 SHA `642a46fd…a9747c`。

Round 2：`executor.rs` 从 542 行降到 468 行，路径校验拆到 `executor/paths.rs`（80 行）；layout 覆盖子模块。

## 未完成

- 23 工具字段映射、必填、format 枚举、count/limit/jobs/wait/gate 边界已覆盖。
- `lib.rs` 55 行；`rmcp_server.rs` 207 行；`commands.rs` 259 行；另有 args/concat 子模块。
- DSH 可选回归脚本已实跑通过；MCP resources/initialize 已验证。
- 商业化/产品化未开始。

## 遇到的问题

- 上一轮 route smoke 曾把空 replay/curl 误判，已修正。
- 本轮 401/405 断言一次通过。

## 下一轮具体行动

1. 全量 gate、打包、AOCI 收尾。
2. 提交并 push 本轮。
3. 下一轮补审计 CLI 读取或 Pipeline 自动化候选。
