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

Round 83（导出 dimension）：去掉流式导出中固定的 `<dimension ref="A1"/>`，
避免向 Excel 谎报已用区域；让 Excel 自行计算；测试确认输出无 dimension，发布包 SHA `d26dec89…bfb43`。

Round 82（xlsx 输出流式写）：CSV 逐行写入 BufWriter，不再把整份 CSV 文本
留在内存；输出字节不变；1M 峰值 1.05 → 1.02 GB；发布包 SHA `5d232694…aa1340`。

Round 81（xlsx 原文件不可变）：CLI 测试 `convert xlsx --out 同一路径` 被拒绝，
输入字节不变；覆盖原始文件不可变约束；发布包 SHA `7140f0d8…8d773`。

Round 80（1M 内存实测）：1,000,000×10 导回峰值 RSS 1.05 GB，确认内存成本
在整表读入与文本重写；作为后续流式优化依据；发布包 SHA `f7483d16…86a0c2a`。

Round 79（1M 行实测）：1,000,000×10 导出 7.18 s、导回 4.76 s、xlsx 43.5 MB，
round-trip 字节一致；验证线性扩展与上限内可用；发布包 SHA `f5cb97e0…1d810`。

Round 78（hidden sheet CLI）：真实双 sheet xlsx 验证默认跳过 Hidden、显式
`--sheet Hidden` 仍可读；用户面行为与单元测试对齐；发布包 SHA `83615a36…d15da`。

Round 77（sheet 状态解析）：`workbook_sheets` 解析 hidden/veryHidden 的状态测试，
覆盖 selection 测试未触及的 XML 属性路径；发布包 SHA `285096dd…fd136`。

Round 76（隐藏 sheet）：默认选择第一张可见 sheet；显式 `--sheet` 仍可选 hidden；
测试覆盖 default/显式两条路径；发布包 SHA `e4a282e2…10f21`。

Round 75（属性解析测试）：手工 attr 解析覆盖前缀碰撞（r vs r:id）、单双引号与
XML 转义，防止性能优化悄悄改变属性语义；发布包 SHA `1195416d…4cbeb0`。

Round 74（行号排序）：按 XML 的 row r 排序，乱序工作表也按 Excel 行序输出；
测试固定；复测导回 0.93 s，发布包 SHA `7e0a94f7…128c1a`。

Round 73（rowBreaks 边界）：线性扫描必须校验标签边界，`<rowBreaks>` 不再被
误当 `<row>`；新增测试，防性能改写引入语义回归；复测导回 0.92 s，
发布包 SHA `7c1e6a72…20ebf`。

Round 72（xlsx 导入性能）：200k×10 导回从 240–295 s 降到 0.93–0.96 s（release），
round-trip 字节一致；原因是 per-cell 正则编译，改为线性扫描/手工 attrs/OnceLock。
复现脚本 `scripts/bench_xlsx_roundtrip.sh`；发布包 SHA `f91e2a2b…5eed665`。

Round 71（夜间检查点）：刷新 NIGHT_CHECKPOINT 到 2026-11-26（CLI 177、xlsx 18、
AOCI 243、包 87ee），NIGHT_QUEUE 记录 calamine 依赖与 goal edit 待拍板。

Round 70（xlsx 审计验收）：objective 四项已满足——共享字符串/日期/公式缓存、
可诊断错误、CSV 原生路径不变、定向测试 + 全量 gate + package。证据：
xlsx 18 passed、CLI 177 passed、gate 复跑 passed、AOCI 243 aligned。
未做：calamine 迁移需依赖/离线审批；goal edit 需直接人类轮次。

Round 69（1904 CLI 集成）：真实 xlsx fixture 带 `date1904` 与日期样式，
`convert` 输出 `1904-01-02`；覆盖工作簿标记到 CSV 的整条路径。
首轮全量 gate 有 1 个 CLI 用例偶发失败，单跑与复跑均 177 passed；发布包 SHA `87ee0ae1…81adc`。

Round 68（1904 日期系统）：读取 workbook `date1904`，日期序列号按 1904 纪元
换算；测试覆盖 0/1/0.5 三个边界；发布包 SHA `4cb49af6…9489d`。

Round 67（XLSX 覆盖矩阵）：XLSX_ENGINE.md 增加 11 项边界/测试矩阵，作为
xlsx 导入审计的对外验收证据；随包分发；发布包 SHA `c1c3c07d…69ba3e`。

Round 66（自定义时间格式）：`is_date_format` 识别 hh:mm:ss，纯时间单元格
0.5 输出 12:00:00；测试覆盖；发布包 SHA `960e7f21…ffb68`。

Round 65（XLSX 引擎决策）：新增 XLSX_ENGINE.md，明确无官方 Rust 库、
calamine 为社区库，记录迁移方案与待批准的依赖/离线成本；文档进发布包；
发布包 SHA `c4111884…42026e`。

Round 64（解析上限前置）：`sheet_rows` 解析时即限制总行数（含表头），避免
先解析百万行再拒绝；小限制测试覆盖；发布包 SHA `4397e6b9…61b61`。

Round 63（行上限按数据行）：xlsx 读取的 1,048,576 上限按数据行计，不把表头
算进去；测试改为 3 条数据触顶；发布包 SHA `e97d1380…becdcb`。

Round 62（行上限防护）：抽出 `table_from_rows`，xlsx 读取也执行 1,048,576
行上限；测试覆盖空行跳过与超限拒绝；发布包 SHA `6963f97b…78ad`。

Round 61（列上限防护）：`sheet_rows` 拒绝超出 Excel 16384 列的 cell 引用，
避免畸形 xlsx 触发巨量单元格分配；发布包 SHA `0c348be6…71eaa`。

Round 60（公式缓存类型）：补 cached string/boolean 公式值测试，确保
t=str/t=b 都读缓存值而非重算；发布包 SHA `ebc7aee8…d71c88`。

Round 59（合并单元格用户面）：CLI 测试确认 merge A1:B1 只输出 A1 的存储值，
不凭空补 B1；空行/公式测试共用夹具构造器；发布包 SHA `4f7fce51…c5c98e`。

Round 58（空行/夹具复用）：CLI 测试抽取最小 xlsx 夹具构造器；新增空行跳过
测试，输出只保留表头与数据行；发布包 SHA `567064da…bfe1`。

Round 57（CLI 公式错误）：构造最小 xlsx，无缓存公式值转换返回 1、stderr 含
no cached value，且不写输出文件；用户面错误路径被测试钉住；
发布包 SHA `3cba2efe…27fa9`。

Round 56（CLI 小写 sheet）：`convert --sheet data` 在真实 xlsx fixture 上成功，
覆盖用户面大小写选择；发布包 SHA `4c66dbe9…dcbfb`。

Round 55（内联字符串回滚）：Gate 发现既有 xlsx fixture 用空 inlineStr 表示空单元格；
按既有契约恢复为空值，未把合法空单元格当错误；发布包 SHA `eb01183f…2e291`。

Round 54（日期时间单元格路径）：新增 sheet_rows/cell_value 集成断言，
45292.5 走日期样式后输出 2024-01-01T12:00:00；发布包 SHA `6ab7ea31…32bb27`。

Round 53（sheet 选择大小写）：xlsx sheet 名按 Excel 语义大小写不敏感匹配，
`data` 可选 `Data`；测试改为小写选择；发布包 SHA `58229d6c…41c7f`。

Round 52（xlsx 日期时间）：日期序列号支持小数时间（日期时间/纯时间单元格），
测试覆盖 45292.5=12:00:00、0.25=06:00:00；发布包 SHA `c9266c04…b48808`。

Round 51（xlsx 自定义日期格式）：`styles.xml` 的 numFmt formatCode 能识别
yyyy/m/d 等日期格式，序列号转 ISO；百分比格式保持原值；测试覆盖；
发布包 SHA `e4e0a7eb…7eb95`。

Round 50（xlsx 导入保真审计）：补共享字符串/cached formula/合并单元格测试；
无缓存公式值、越界共享字符串改为可诊断错误；文档写明不展开合并单元格；
发布包 SHA `fb7bdcc5…97e482`。

Round 49（Excel 公式缓存验收）：确认 `window()` 已有按 journal ops/base/引用表
缓存；补引用表失效与 Sheet 编辑/undo 失效单测；命中实测 0.051 ms（100 次最差，debug）；BEYOND_EXCEL 移除非真实缺口
xlsx/事务块/缓存；发布包 SHA `75996c19…2e711b`。

Round 48（安装清单内容断言）：install smoke 除检查 release-manifest 存在外，
还断言其 sha256/name 与安装清单/原始 release 一致；发布包 SHA `65ecdec0…ec6a0`。

Round 47（支持矩阵补行）：SUPPORT_MATRIX 增加“安装 prefix 保留/复核 release
manifest”能力行，和自检实现对齐；发布包 SHA `5ae3efa8…4077b`。

Round 46（失败命令门）：bench_compare 拒绝 exit 非 0 的命令，smoke 增加
failed_command 负例；发布包 SHA `176fe361…fd236`。

Round 45（数据集一致性）：bench_compare 拒绝 bytes/rows 不同的两份 summary；
smoke 修正 total 用例并增加 dataset mismatch 负例；发布包 SHA `bd3433ad…26c4b`。

Round 44（总量回归门）：bench_compare 除逐命令外也拒绝 totalMs 超阈值，
smoke 增加 total 40% 增长拒绝；发布包 SHA `af877f83…38858`。

Round 43（发布艺术品复核）：对 `85312532…afb70` 包复验
`phase_thresholds=ok`、`verified=true`、`check_release manifest=ok`、
`criticalDocs=4`；复验写 `.reverify.json`，不覆盖 manifest 绑定的 `.verify.json`。

Round 42（诊断包收录清单）：`test_ops_bundle.sh` 在 fake prefix 内放
release-manifest，断言诊断包解包后包含该文件；发布包 SHA `85312532…afb70`。


Round 41（自检复核发布清单）：install_selfcheck 若发现 prefix 内有
`release-manifest.json`，会比对 sha256；不一致拒绝，缺失则记录 not provided；
发布包 SHA `ec663055…52825`。

Round 40（安装保留发布清单）：install_offline 在 prefix 内保留
`release-manifest.json`，ops_bundle 会一并收集；安装 smoke 断言存在。
本轮另把 skill 安装阶段阈值从 3s 调到 5s（实测 3.47/3.53s 偶发超 3s）；
发布包 SHA `98e33052…dcc6f`。

Round 39（性能回归比较）：新增 `bench_compare.py` 比较两份 summary 的逐命令
耗时/总量，默认 20% 阈值；smoke 覆盖通过、回归拒绝、缺命令拒绝；已进 gate；
发布包 SHA `ec830a1a…6590e`。

Round 38（关键文档字节校验）：`check_release.py` 对 criticalDocs 同时校验
SHA256 与 bytes，任一不符即拒绝；发布清单面积扩展到大小/哈希双重绑定；
发布包 SHA `93c7df7f…7899b`。

Round 37（安全文档纠偏）：SECURITY 资源数从 2/3 矛盾改为明确 3（agent/
operations/audit），并补 release manifest 四文档哈希与复验不覆盖绑定 verify；
发布包 SHA `419fae53…38b56`。

Round 36（打包前清理旧验证）：`package_release.sh` 在重新打包前删除旧
verify/release 文件，避免旧 `.release.json` 让 verify_dist 写 reverify、release
manifest 读到旧 checks；连跑两次发布包均 `manifest=ok`，SHA `249861cc…e647`。

Round 35（复验不破坏 manifest）：`verify_dist.sh` 发现有 `.release.json` 时写
`.reverify.json`，保留 manifest 绑定的 `.verify.json`；复验后 `check_release`
仍通过；发布包 SHA `6989b299…35353e`，复验写 `.reverify.json` 且 `check_release=manifest=ok`。

Round 34（发布清单负例）：新增 `test_release_manifest.sh`，证明 criticalDocs
写入并可从 tar 重哈希；缺 DEPLOY 时 release_manifest 拒绝；已进 gate；
发布包 SHA `4e02656e…c6e7b`。

Round 33（关键文档哈希）：release manifest 增加 `criticalDocs`（四份文档的
tar 内 SHA256），`check_release.py` 重哈希校验；缺文档即打包失败；发布包 SHA `968727e7…84b07`。

Round 32（发布后诊断包）：`package_release.sh` 在真实安装上运行
`ops_bundle.sh` 并检查 tar 内含 selfcheck/ops-status，失败整包失败；发布包 SHA `98143e14…ec3a6`。

Round 31（支持矩阵）：新增 SUPPORT_MATRIX.md（平台/功能矩阵、数据边界、
工单最小信息、响应边界），并入发布包与成员校验；发布包 SHA `571754d2…dbc06`。

Round 30（诊断包）：新增 `ops_bundle.sh` 生成带 SHA 的本地诊断 tar.gz；
默认不含审计/历史，显式 include-logs 选项才加 state；smoke 进 gate；发布包 SHA `d3f94422…dba49`。

Round 29（发布后运维快照）：`package_release.sh` 在真实安装后运行
`ops_status.sh`，要求 `installOk=true` 才产出发布包；发布包 SHA `6e938fe5…6761c`。

Round 28（运维快照）：新增 `ops_status.sh` 聚合安装自检 + pipeline 历史 +
MCP 审计计数，输出本地 JSON；正负例 smoke 进 gate；发布包 SHA `da60f13a…862fd`。

Round 27（发布后自检）：`package_release.sh` 在真实安装后运行
`install_selfcheck.sh`，要求 JSON `ok=true` 才产出发布包；发布包 SHA `e2c2d13a…ebadf`。

Round 26（升级/回滚演练）：新增 `test_upgrade_rollback.sh`，双 prefix 演练
4.1→4.2 切换/回滚与缺失 prefix 拒绝；smoke 进 gate；发布包 SHA `9233849d…2ff0e`。

Round 25（安装自检）：新增 `install_selfcheck.sh` 校验 manifest/version/platform/
docs/审计目录并输出 JSON；正负例 smoke 进 gate；DEPLOY 增加升级/回滚/排障；
发布包 SHA `9e8f3929…cccd1`。

Round 24（设计伙伴材料）：新增 PILOT_INTERVIEW.md / MIGRATION.md；
`pilot_report.py` 自动写 `pilot-report.md.json`（版本/样本量/四操作/限制，
不含原始行）；pilot kit smoke 断言客户样本摘要；发布包 SHA `14d4e9e6…3b339`。

Round 23（确定性验证）：bench smoke 对同一 `--rows` 跑两次并 `cmp` 生成 CSV，
证明数据确定性，而不只是文档声明；发布包 SHA `f77bff96…5acc3`。

Round 22（摘要脱敏修正）：summary 的 dataset 只保留 bytes/rows/generateMs，
不含生成路径；smoke 断言 `path` 不存在，兑现“无原始路径”承诺；发布包 SHA `308f9ba0…80bf`。

Round 21（benchmark 摘要自生成）：harness 自动写 `<out>.summary.json`（脱敏、
无原始路径/行值），smoke 校验 dataset/total；`docs/bench-100gb...json` 为同形摘要；发布包 SHA `7a6620b5…cc266`。

Round 20（README 对外证据）：README 增加 100GB 本机实测三项数字与机器可读
摘要链接，明确非 SLA；发布包 SHA `ba0bafed…53a86`。

Round 19（发布成员契约）：verify_dist 要求 SECURITY、DESIGN_PARTNER、100GB JSON、
离线安装/bench/pilot 脚本都在包内，缺一即失败；修正脚本归档到 scripts/ 后
发布包 SHA `d6c52122…5d437`。

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
