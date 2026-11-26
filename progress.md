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

Round 18（benchmark 可读摘要）：新增 `docs/bench-100gb-2026-11-26.json`，
只含机器/数据量/四命令 elapsed/RSS，不含原始行；随发布包分发；发布包 SHA `3cb1887a…07677`。

Round 17（benchmark 机器信息）：BENCHMARKS 写明本机 Apple M5 / 16 GiB /
macOS 27.2.1 arm64，100GB/1GB/smoke 均标注同机实测；发布包 SHA `5a778439…b60f5`。

Round 16（压缩 intake）：`pilot_intake.py` 支持 `.gz`/`.bz2`/`.zst` header；
zstd 通过本机 `zstd -dc` 只读首行；smoke 覆盖三种压缩；发布包 SHA `9f0d5b3e…99f9e`。

Round 15（商业材料）：BUSINESS.md 增加“已落地地基”表，明确审计/配置/Pipeline/
离线安装/100GB 证据，并重申无收费门控、无 License、无遥测；发布包 SHA `a625af15…ad2c6`。

Round 14（发布安装 smoke）：`package_release.sh` 在真实 tar.gz + release.json 上
执行离线安装并验证安装后二进制；失败则整包失败；发布包 SHA `d5800167…9ebe8`。

Round 13（真实样本条件）：`pilot_kit.sh` 对 `CSVX_PILOT_FILE` 不再硬编码
`city=CDMX`/`group city`；可用 `CSVX_PILOT_WHERE/KEY/GROUP`，自定义样本报告验证
`过滤条件：(未指定)`；发布包 SHA `bdb50034…b06ae`。

Round 12（试点真实样本）：`pilot_kit.sh` 支持 `CSVX_PILOT_FILE` 对已脱敏样本生成
报告与 intake；`CSVX_PILOT_MAX_GB` 默认 20；smoke 覆盖客户样本与 email 警告；发布包 SHA `97fc2acf…3f8da`。

Round 11（离线安装 manifest）：`install_offline.sh` 支持可选 release manifest 复核
release JSON 的 sha256/bytes；安装 manifest 增加 platform；smoke 覆盖 bad manifest 拒绝；发布包 SHA `6a431bf2…47b9`。

Round 10（intake 健壮性）：无法读取 header 的样本写 `error` 并以 2 退出；
smoke 覆盖 2 MB 单行畸形 CSV，不再抛出 traceback；发布包 SHA `61a56094…69db4`。

Round 9（试点 kit 整合）：`pilot_kit.sh` 现在同时生成 `intake/intake.json/md`，
kickoff JSON/MD 引用 intake；smoke 覆盖六件产物；发布包 SHA `176b6660…45d75`。

Round 8（设计伙伴 intake）：新增 `scripts/pilot_intake.py`，只读 header/大小并检测
可疑敏感列名，输出 intake JSON/MD，不落原始行；smoke 纳入 gate；发布包 SHA `0e8b7002…d310f`。

Round 7（设计伙伴支持）：新增 `scripts/pilot_kit.sh`（demo + pilot report +
kickoff JSON/MD，无网络）与 smoke；gate 增加 pilot-kit-smoke；新增
`docs/DESIGN_PARTNER.md`（4 周计划、指标、数据边界、非目标）；发布包 SHA
`61cfc2cd…4ac72`。

Round 6（Benchmark 目标）：新增 `scripts/bench_100gb.py`（流式生成、schema/filter/
search/quality、elapsed+RSS、JSON+Markdown）与 smoke 脚本；gate 增加 bench-smoke；
1000 行 smoke 1348.3 ms；1 GB 实测 total 5727.7 ms；**100 GB 实测**
99,999,999,969 bytes / 1,259,779,856 rows：schema 70.19s、filter 23.55s、
search 18.72s、quality 456.00s、total 568.46s（quality 约 80%）；发布包 SHA `85de6c1d…bfe28`。

Round 5（企业部署目标）：新增 `scripts/install_offline.sh`（SHA256→解包→demo
smoke→prefix/bin + docs→`install-manifest.json`），坏 SHA 与版本不符拒绝；
新增 `scripts/test_install_offline.sh` 并进入 gate；新增 `docs/SECURITY.md`
（逐条引用已有实测证据）；发布包 SHA `f067b28b…cce94b`。

Round 4（MCP 审计查询目标）：`csvx mcp audit` 支持 tool/errors/limit/json；
缺文件成功退出；元数据含 bytes/rotated/max/total/shown；3 个单测 + 1 个 CLI
集成测试；文档 §7；发布包 SHA `7640beaa…c52e9`。

Round 3（MCP 配置目标）：`csvx mcp config --client dsh|claude|cursor|generic`
输出可粘贴 JSON；默认当前 exe 绝对路径；未知 client 列出合法值；3 个单测 + 1 个
CLI 集成测试；文档写明粘贴位置；发布包 SHA `4cff7664…55ac0`。

Round 2（Pipeline 自动化第二阶段）：history 增加 `event:"attempt"`（attempt/ms/
ok/code/retryDelayMs），失败重试可审计；`pipeline status --verify` JSON 增加
`staleReason`（input/output missing、hash changed、stamp changed 等）；发布包 SHA `5bfa79ef…5d543`。

Round 1（Pipeline 历史目标）：每轮 `pipeline run` 追加 `.csvx/pipeline-history.jsonl`；
CLI `pipeline history` 可读；3 个历史单测 + 1 个 CLI 集成测试；默认 4 MiB 轮转。
失败记录 `pipeline_error`，fresh 步骤记 `skipped`；发布包 SHA `8170577b…64abb`。

Round 1（审计目标）：MCP `call_tool` 落盘审计已实现，默认 `.csvx/mcp-audit.jsonl`；
参数仅记录 key/type/长度，4 MiB 轮转；`csvx://audit` 可读（NDJSON，文档见 MCP_COMPAT §3）；新增 3 个单测，
MCP 单测 11；SDK `resources=3 audit_entries=63 audit_redacted=true`；发布包 SHA `6fd23f65…9f9a9`。

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
2. 提交本地；本轮 push 已用于补推前一批，下一轮再推。
3. 目标保持 active，继续试点支持。
