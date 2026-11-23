# 交付说明 / Handover

**版本 4.2.0**：一个 CLI + HTTP 后端（**没有 GUI**）。这份文档说明它是什么、能给前端
什么、边界在哪、怎么接着做。验收对照见 [`ACCEPTANCE.md`](ACCEPTANCE.md)。

---

## 一句话

**一个不重写原文件的 CSV 表格引擎**：把几十 GB 的表当电子表格改，内存只跟编辑操作数
和一屏的行数有关，原始文件逐字节不变，导出时才物化。

```bash
csvx demo -o .          # 生成一个真实感的示例文件
csvx view orders.csv --rows 20
csvx edit set orders.csv --row 3 --col city --value CDMX
csvx edit formula orders.csv --name vat --value "amount*0.16"
csvx edit export orders.csv -o clean.csv
```

## 一条命令演示（周一用这个）

```bash
cargo build --release
scripts/demo.sh --big /tmp/csvx_big/big.csv --writes   # 功能巡演 + 2.1 GB 写路径
scripts/demo.sh --big /tmp/csvx_big/big10.csv          # 11.2 GB 读路径
```

本机实测（release，多次运行取范围）：2.1 GB 建索引 0.7–1.5 s、打开/跳转 0.01 s、过滤 2.3 s、
搜索 2.4–2.6 s、编辑 0.01 s、导出 1.7–1.8 s、verify 5.8–6.2 s；11.2 GB 索引 3.5–3.8 s、
过滤 13.2–13.9 s、搜索 13.8–14.6 s；2026-11-23 复测 11 GB/2.8 亿行过滤 14.9 s、搜索 16.3 s——耗时随行数而非字节数增长，估容量先看行数。
演示完 `rm -rf /tmp/csvx_big` 回收约 13 GB。端口被占也不怕：demo 会向系统要空闲端口。

---

## 给前端的三个接口 / the framework

前端只需要问三件事，这三件都有库、CLI、HTTP 三层：

| 问题 | 库 | CLI | HTTP |
|---|---|---|---|
| 有哪些列、这一列在不在 | `sheet::schema()` / `column_index()` | `schema --has`（存在 0 / 不存在 1）| `GET /columns`（全列）+ `?name=`（**2.8 ms** 于 782 列文件）|
| 这一屏是什么 | `Pager::page` + `Window.keys` | `view --columns` | `GET /rows`（1-based、深偏移**常数时间**、`rowKeys`、`count/requested/limit`）|
| 我能安全地改吗 | `expect` 比较后再写 | `edit set --expect` | `POST /edit?action=set&expect=` → 不符 **409** 带 `actual`/`expected` |

细节见 [`INTEGRATION.md`](INTEGRATION.md)（含错误码速查与并发约定），字段含义见 [`API.md`](API.md)。

## Journal 压实（阶段 2，设计与基线）

**基线（2026-11-23，1M 行 base，release）**：journal 100 万条（22 MB）：view 0.46 s / export
1.00 s / undo 0.51 s；1000 万条（222 MB）：view **4.84 s** / export **4.61 s** / undo **5.50 s**。
耗时随 journal 行数线性增长；1M 条以内还可用，千万级已明显拖慢打开与回滚。

**为什么不能直接把 op 折进 base**：`Op::SetCell` 只存新值，不存旧值；现有 undo 的做法是从
journal 尾部弹出一条 op，然后以 base 为底重放剩余 journal。若把已应用的 op 折进 base 并清空
journal，undo 就失去恢复旧值的依据。因此压实必须同时保留“可回退的历史”，而不是只留当前状态。

**选定方案（显式 `edit compact`，一次一层，保留 base 原始字节）**

1. 前置条件：redo 为空；先写临时快照，fsync。
2. 原 `b.csv` 改名为 `b.csv.base`（原始字节完整保留，base 仍然只读）；journal 改名为
   `b.csv.journal.history`，blocks 一并移动；快照原子替换 `b.csv`，当前 journal 清空。
3. 所有读者路径不变：`b.csv` 现在是快照；新编辑仍写 `b.csv.journal`（相对快照的 row 0）。
4. **undo 跨界**：当前 journal 为空且 history 存在时，先把 `.base` 和 history 换回，再执行原来的
   尾部 undo；redo 继续按原语义工作。若 redo 非空则拒绝跨界的 undo（因为 redo 对应快照后的状态，
   换回会使其失效），错误里说明原因。
5. **崩溃恢复**：压实前写标记（含快照 sha256、原 base、history 路径）；打开时校验当前 base 的
   sha256：若已是快照，则清空 journal/删标记；若仍是原 base，则删标记保留 journal。标记损坏时
   拒绝自动修复，提示用户运行 `edit repair`。
6. **sidecar**：`.idx` 随原 base 改名为 `.base.idx`，快照下次访问重建；`.view` 保留（列布局与快照
   无关，行数由 journal 决定）。
7. 不做隐式自动压实；先显式命令，等崩溃/并发测试稳定后再评估阈值。

**已实现（2026-11-23，`edit compact`）**：CLI 压实、标记驱动的恢复（未安装快照则回滚、已安装则
清空旧 journal）、跨界 undo/redo 还原；压实全程持有 journal 写锁，`update_locked` 的并发写会等锁。
实测 1M 行 base + 100 万条 journal（22 MB）：压实前 view **1.13 s** / export **0.91 s**，压实
**1.04 s**；压实后 view/export **0.02 s / 0.02 s**；原 base 与 history 保留（18 MB + 22 MB）。

测试：CLI 端到端钉住“压实 → 两次 undo 跨边界 → 两次 redo 回到快照态”，并比对压实前后导出
**逐字节一致**；`CSVX_COMPACT_CRASH_AT` 在 after_copies / after_prepared / after_rename /
after_install 四个窗口真实 abort 进程，下一命令都能恢复且编辑只应用一次；单测覆盖安装前回滚、
安装后补清空、锁等待、已有快照时拒绝且不改文件。

2.2 GB / 5500 万行 drill（2026-11-23）：index 1.86 s；压实 **1.77 s**、RSS **8 MB**；
压实前后 export **1.76 s / 1.68 s** 且逐字节一致；压实后首次深跳重建索引 0.82 s，之后 0.01 s；
跨界 undo **0.03 s**、redo 0.01 s，编辑在正确行上被移除/恢复。`.idx` 随原 base 归档为
`.base.idx`，跨界 undo 再换回；快照写入失败（temp 被目录占用）时 base/journal 不变。

1000 万条 history（227 MB）压实（2026-11-23）：压实前 view **9.27 s** / export **7.29 s**，
compact **8.98 s**、RSS **8 MB**；压实后 view/export **0.02 / 0.03 s**；副本 18 MB + 218 MB；
前后 export 逐字节一致。

真实 ENOSPC（300 MB HFS+ 卷，填至 3 MB 空闲）：compact 退出 1、`os error 28`；base/journal
sha256 不变、journal 仍 2 行；快照失败会删除 partial `.compact.tmp`，无 marker/archive 残留。

并发写：`CSVX_COMPACT_SLEEP_MS` 让压实持锁停 800 ms，此时启动的 `edit set` 等待并在快照落地后
成功；导出同时包含压实前编辑与等待编辑，行数不变。`.view` 布局在压实与跨界 undo 后都原样保留。

**阶段 2 已完成**：打开时间不再随已折叠历史增长、跨界 undo/redo 正确、崩馈四窗口可恢复、
真实 ENOSPC 不破坏文件、CLI 并发写不丢。HTTP 写走同一 `update_locked` 锁路径，但未单独做
serve 端到端压测；`.idx` 已随 `.base` 归档并在跨界 undo 恢复。

## Book 多表清单（阶段 3，已完成）

`book --plan book.csv --root .` 用 CSV 清单声明 sheet/lookup/output，按名字链式查找并物化输出；
未知表/未知 lookup/重复名在写任何输出前报错，重跑覆盖同一路径（幂等），不做 `Sheet!A1`。
2.1 GB / 5500 万行 drill（2026-11-23）：两条链式 lookup 输出 3.1 GB，**46–48 s**、峰值 RSS **15 MB**；
55,000,001 行，首行 `o1,...,Customer-1,Region-1`；重跑 sha256 一致、无 `.booktmp` 残留。
慢于单次 lookup 的原因是要落一次中间文件（链式两遍读写）；功能正确性与内存已达标。

## 质量报告（阶段 4，已完成）

`analyze FILE --quality [--key col] [--group col] [--json|--markdown]`：候选键、重复/空键、
mixed-type、数值 mean ± 3σ 离群估计、分组组数/最大组占比/空组；单组或占比 ≥80% 告警。
固定 fixture 的 JSON/Markdown 期望值已入 CLI 测试；2.03 GB / 5600 万行 drill：**18.71 s**、
RSS **17 MB**，`key` 报 55,999,000 重复、`city` 报 5,600,000 空值，候选键为空（id 被 cap 截断）。

## 格式互操作（阶段 5，已完成；xlsx 挂起）

`convert f.csv --format tsv|ndjson|parquet -o out` 与 `convert out.parquet --format csv -o back`
覆盖 CSV/TSV/NDJSON/Parquet 任一方向；DuckDB 负责解析与写出，引号字段、Unicode 保持。
round-trip CLI 测试三种格式均回到逐字节相同的 CSV；`--format xlsx` 明确报错且不写文件。
2.2 GB / 5500 万行 drill（2026-11-23）：CSV→Parquet **5.36 s**、输出 224 MB、RSS 581 MB；
Parquet→CSV **2.15 s**、RSS 424 MB；返回 CSV 与原始 CSV **逐字节一致**、55,000,001 行。
xlsx 仍按硬约束挂起，等用户明确解除。

## 100 GB 引擎（C 主线，首轮 profile 达标）

104.5 GB / 13 亿行合成文件：preview 0.02 s、index 24.57 s（9.9 MB 索引）、search 20.2 s/93 MB、
filter 25.75 s/95 MB、选择性 filter -o 19.54 s、深跳 0.02 s。扫描目标 search <60 s、
filter <90 s、RSS <200 MB 满足；全量 filter -o（200M 行/20 GB）98.56 s，仍超 90 s；part+final 双写
是主成本，并行区间写入仅省 3.4 s。100 GB 仍是重复块。复现见 `docs/BENCHMARKS.md`。

## MCP 后端（B 主线，只读首版）

`mcp --root DIR`：stdio JSON-RPC；读工具 `csvx_schema/view/filter/search/quality/pipeline_status`，
写工具 `csvx_edit_set/delete/undo/redo` 与 `csvx_convert` 复用 journal 锁与 root 限制。
CLI 测试覆盖 initialize、tools/list、schema/view/filter、edit set→view→undo、convert TSV，
以及异步 job 提交/轮询（done 结果缓存）。2.3 GB/9500 万行 MCP drill：cold schema 3.11 s、warm 0.03 s、深分页 0.02 s、filter 0.77 s、
search 0.56 s、quality 22.25 s；响应 0.4–4 KB。下一步：job 状态与真实客户端联调。

## Pipeline 工作台（A 主线，进行中）## Pipeline 工作台（A 主线，进行中）

`pipeline run plan.csv --root .`：CSV 清单（step/input/command/args/output）顺序执行，
复用现有命令；`{out}` 写临时文件后 rename，失败不留 partial；未变步骤按输入/参数/输出
哈希跳过，`--force` 全跑；`.pipeline.csv` 即血缘并记录每步耗时。CLI 测试覆盖首次运行、
第二次全跳过、输入变更后重跑、坏步骤不写目标/状态、前向引用拒绝。

2.3 GB/9500 万行 drill：filter→lookup→groupby 总 6.55 s；filter 0.65 s/29 MB，SQL lookup
0.70 s/244 MB（1900 万行全 join），groupby 0.40 s/194 MB；增量重跑 **17 ms**（size+mtime
快路径，文件变了才重算哈希）。drill 同时发现并修复 SQL lookup 硬编码 100,000 行截断。

## 对 Excel / WPS 的覆盖

| 电子表格操作 | 这里怎么做的 |
|---|---|
| 改单元格 | `edit set --row --col`（1-based，可 `--where` 批量）|
| 增删行列 | `edit insert-rows` / `delete`（按行、按条件、**按校验规则**、按内容）/ `insert-column` / `drop` / `rename` |
| 虚拟列 | `edit formula`：`lag()`、`running_total()`、A1 引用、`ref()/vlookup()/xlookup()` 跨表（xlookup 可给 fallback） |
| 查找替换 | `search`（含 `--regex`）、`edit replace`（文本 / 正则 / 按列 / 第 n 个）|
| 撤销重做 | `edit undo` / `edit redo`（journal 的栈，跨进程有效）|
| 排序 | `sort`（跨块归并，稳定），或 `edit export --order` |
| 筛选 | `filter --where`、`view --where`、`/rows?where=` |
| 透视表 | `pivot`（行×列×聚合，一次流式扫描）|
| 分组汇总 | `groupby`（sum/mean/min/max/count/distinct…）|
| 条件格式 | `format --rule "amount>50000:bg=red"`（服务端求值，返回可上色范围）|
| 图表数据 | `chart`（类目 + 序列，终端还能画条形图）|
| 列宽 / 冻结 / 数字格式 | `layout`（存 `.view`，与数据分离）|
| 数据校验 | `validate`（非空/数字/整数/日期/唯一/比较，日期按真实日历）|
| 分列 / 合并列 | `edit split-column --by "," --into a,b` |
| 跨表引用 | `lookup`（VLOOKUP）|
| 拼接 | `concat`（表头不一致会拒绝）|
| SQL | `sql`（DuckDB，只读，超时保护）|
| 拆分上传 | `split`（多线程、压缩、可 `verify`）|
| 给前端用 | `serve`（21 个读端点 + 11 个写动作 + 内置查看页）|

**不做的**：图形渲染（只给数据）、多 sheet 公式联动（`ref()` 是只读查找）、
真正的 xlsx/xls 读写（这是 CSV 引擎）。

## 三条承诺，各自的证据在哪

| 承诺 | 证据 |
|---|---|
| **① 原文件不可变** | 测试 `no_command_modifies_the_input_file`：哈希+大小+mtime，覆盖每条写命令，含一条故意失败的 |
| **② 内存与行数无关** | `docs/CAPABILITIES.md` 的全命令 RSS 实测表；500 万行 + 公式列导出峰值 **17.3 MB** |
| **③ 会骗人的地方不静默** | 错误契约测试、`truncated` 标记、`#VALUE!`/`#N/A`、journal 损坏警告、四种路径错误、`troubleshooting_quotes_are_real` |

**② 的准确版本**：与行数无关，**除非那个命令必须记住每一行**——`dedupe` 记键、
`sort`/`partition` 有缓冲；三者都有显式上限，超了报错。

## 性能（215 MB / 500 万行，release）

| 场景 | 时间 | 内存 |
|---|---|---|
| 建索引 | 0.08 s | 16.3 MB |
| 跳到任意行 | 0.01 s | 7.8 MB |
| 看一屏 | 0.01 s | 8.5 MB |
| 全列 analyze | 1.66 s | 17.2 MB |
| 分组聚合 | 1.39 s | 9.0 MB |
| 拆 8 片 / 4 线程 | 0.40 / 0.18 s | 90 / 120 MB |
| 合并 | 0.18 s | 13.3 MB |
| 套用编辑导出 | — | **17.3 MB** |

API：`/rows` 首屏 66 ms（含建索引），之后 **6–7 ms**，任意偏移一样快。

## 改一处，要同时改哪里（现在都有测试盯着）

这些是最近十几轮补上的守卫。**列在这里是因为它们会失败**——忘了同步就会红，而不是
靠记性。

| 你改了 | 同时要看 | 守着它的测试 |
|---|---|---|
| 任何写 journal 的路径 | 必须走 update_locked，不要自己 load+save | serve 的 concurrent_http_edits_all_land（8 并发写）|
| 新增一个渲染端点 | 归入「看得到编辑」或「只读基文件」两榜 | serve 的 the_journal_visibility_matrix_holds |
| 新增一个支持引擎的端点 | 结果等价 + 错误等价各一条 | engine 里五个 *_engines_agree_* |
| 新增/改请求参数 | SPEC_PARAMS；只对写侧有意义的另记 WRITE_ONLY_PARAMS | /spec 的 contract 测试、writeOnlyParams 测试 |
| 新增 HTTP 状态码 | STATUS_NOTES | contract 的 spec_documents_every_status_the_server_can_return |
| 文档里改响应字段 | 字段必须真的返回 | serve 的 documented_fields_are_fields_the_endpoint_returns（覆盖 health/meta/metrics）|
| 改版本号 | CHANGELOG 顶部同步 | docs 的 the_changelog_leads_with_the_current_version |
| 在文档里加反引号 | 只放命令名或**响应字段**；参数名写成纯文本 | docs 的 api_reference_names_only_parameters_the_server_reads（把反引号里的参数当「文档声明」校验）|

CAPABILITIES.md 里的反引号会被当作命令名校验，**参数名和标志不要加反引号**；HANDOVER
这里同理，测试名写成纯文本。

## 已知限制（写明，不暗示）

1. `running_total` 在窗口视图里是 `#VALUE!`（它需要起点之前的每一行）。
2. 需要读别的行的 A1 引用返回 `#VALUE!`；只有同行引用可用。
3. `sort` / `concat` / `sql` 不接受 `--quotes`。
4. `dedupe`/`concat`/`columns`/`split`/`partition` 读**文件**而非编辑后的表；
   `analyze`/`stats`/`filter`/`search`/`export`/`lookup`/`sort` 默认读文件，但都有 `apply-edits=1`。
5. `dedupe` 内存随不同键数增长；`sort`/`partition` 随 `--buf-mb` 增长。
6. 内置查看页**服务端已验证**：`GET /` 返回 200、13 KB HTML、标题正确（脚本可复现）；
   但交互（点击、翻页、编辑）未在真实浏览器里跑过。
   **Windows 由 CI 覆盖**：`.github/workflows/windows.yml` 在 windows-latest 上编译并检查
   Rust 变更（路径过滤，文档提交不触发）；最近一次对应 6e910d5 的结论为 success。
   注意：**改版本号也算改 Cargo.toml**，所以每次发版都会触发这条约十分钟的 DuckDB 编译。
   这是**实际耗时**而非费用：仓库为公开仓库，Actions 分钟免费；且我们的纪律是不等 CI。
   因此不改路径过滤——依赖变更（Cargo.toml/Cargo.lock）必须留在 Windows 检查范围内，
   否则真正会在 Windows 上炸的东西反而漏检。
7. 查询有 **30 秒**上限（引擎中断），不是无限等待；`/rows?where=` 在 2.5 GB 上首屏约 8–9 秒。
8. 并发：写入有文件锁（不会丢编辑），但**两次编辑不是事务**；`expect=` 只挡「你读完之后被改」，
   `stamp` 是变更探测器而不是版本号。

## 怎么接着做

| 你想做 | 先读 |
|---|---|
| 改编辑模型（行/列身份、投影顺序）| `docs/ARCHITECTURE.md` 的「编辑模型」+ `workbook/rows.rs` |
| 加一条命令 | ARCHITECTURE 的「改一处要看哪里」：帮助表、README、CAPABILITIES、`scripts/regress.sh` —— **四处都有测试盯着** |
| 加一个 API 端点 | `docs/API.md`、`/sheet` 的 capabilities、`EDIT_ACTIONS` |
| 改内存行为 | 用 `/usr/bin/time -l` 复测，数字写进 CAPABILITIES |
| 不写代码、只验证 | `scripts/dev.sh gate`（fmt+clippy+**606 测试**，另 336 条在 core 的 sql 特性下）、`smoke.sh`（49 步）、`regress.sh`（44 项）、`backend-smoke.sh`（**83 项**，起真服务）、`bench.sh` |

**日常开发节奏**（用户明确要求）：
`scripts/dev.sh check`（约 3 秒）→ 相关单测（约 5 秒）→ 攒够 1–3 个提交跑一次
`gate`（约 90 秒）→ 每轮最多推一次。**不要为一轮验证把时间耗在编译上。**

## 迭代中累积的教训（按价值排序）

1. **每一步都对，连起来就卡住。** 改名丢编辑、分列片被邻居覆盖、填充在删行后滑位——
   三个 bug 都是「两个正确的部分相遇」。单独测每个功能永远发现不了，所以现在有一个
   走完整旅程的测试。

2. **同一个事实写在两处，迟早会漂。** 能力清单三份、命令文档两份、路由清单两份、
   API.md 被 changelog 覆盖了 29 个版本。解法不是「记得同步」，而是**从源码提取事实再比对**。

3. **同一个承诺，两条路径实现，测试只覆盖了一条。** 「翻页常数时间」API 做到了、
   CLI 没有（深页码 5 s）；「内存与行数无关」导出路做到了、`verify` 没有（20 GB）。

4. **`str.replace` 找不到匹配时静默通过。** 五个版本号和一条 changelog 就这样丢过。
   现在每次替换都 `assert` 匹配到了。

5. **注释承诺不存在的调用者是负债。** 两个死函数各自声称「为兼容保留」「在工作线程里用」，
   两者都不存在。删代码时连注释一起删。

6. **验证轮即使没找到 bug 也不亏。** 承诺从「我们相信」变成「有测试」，这才是 2.0.0 的理由。

## 仓库结构

```
crates/core/   引擎：编辑模型、公式、查询、拆分、格式（40 个模块，19600 行）
crates/cli/    30 条命令 + HTTP API + 内置查看页
crates/cli/    命令行 + HTTP 后端（前端由别的项目接）
docs/          README 之外的 7 份文档，6 条一致性测试链
scripts/       dev.sh / smoke.sh / regress.sh / backend-smoke.sh / bench.sh / clean.sh
```


## 认知层：AOCI 与 CodeGraph

仓库里有两层可复用的「系统记忆」，都不需要重新读一遍全部源码：

| 层 | 文件 | 用途 | 维护方式 |
|---|---|---|---|
| **AOCI** | `aoci.txt` / `aoci.meta.txt` / `aoci.code.txt` | 89 条 Entry，每条一个受管理文件：F 职责、R 强关系、A 对外契约、S 不可推断的约束 | 改动受管理文件后，调 `aoci_maintain` 拿批次、写 Entry、再 `aoci verify` |
| **CodeGraph** | `.codegraph/`（本地，不入库） | 2 295 个符号、9 882 条边的代码图，用来查调用链与影响面 | `codegraph init` 或 `codegraph explore "<符号>"`，随时可重建 |

两侧的协作方式：

- **先问认知层「这是什么」**，再读具体源码「它是怎么写的」——AOCI 的 S 字段就是为此存在的：
  它记录那些「不看就会踩」的约束（例如 journal 是唯一承载编辑的文件、UTF-16 不能建索引）。
- **改完之后用 `aoci_maintain` 收尾一次**，不要逐次修改逐次维护；它按仓库状态签发批次，
  未对齐时会明确说还差哪几个文件。
- 索引本身也受版本控制：`aoci*.txt`、`.aoci/{config,baseline}.json`、`.gitattributes`、`AGENTS.md`
  都要入库；`.aoci` 的运行时产物与 `.codegraph/` 不入库。

## 一句话交接

**它现在能把几十 GB 的表当电子表格改，而且三条承诺都有测试。**
下一步最该做的不是加功能，而是拿真实数据跑一遍——**迭代中最有效的发现方式，始终是
「真的执行那个组合」。**

## Running the gate

Each stage prints `== <name> ==` and ends with either `== <name> ok ==` or
`== stage <name> failed ==`, so a failure names itself instead of leaving a log to grep.
`GATE_FAIL_TEST=1 scripts/dev.sh gate` injects a failing stage to prove that naming works.

`bash scripts/dev.sh gate | tail` reports **tail's** exit status, not the gate's — a failing
clippy run then looks green to anything chained with `&&`. Use `scripts/gate.sh` instead: it
captures the status, prints `== gate exit=N ==`, and exits with it. `regress.sh` asserts that
the wrapper still fails when it should.
