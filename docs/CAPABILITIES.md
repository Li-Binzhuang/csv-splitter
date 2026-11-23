# 能力清单 / Capabilities

一页看完这个引擎能做什么、不能做什么、以及每个能力的边界在哪里。
命令行和 HTTP API 是同一套能力的两个入口。

## 顶层概念

| 概念 | 是什么 |
|---|---|
| **base** | 原始 CSV 文件，**永不修改** |
| **journal** | `<file>.journal`，追加式操作日志，记「单元格是什么」 |
| **view** | `<file>.view`，列宽/显示名/冻结/数字格式，记「单元格长什么样」 |
| **index** | `<file>.idx`，稀疏行索引，让跳转到任意行是 O(1) |
| **sheet** | base + journal 呈现出来的一张表 |
| **book** | 一个目录下所有 CSV 组成的工作簿 |
| **parquet** | `<file>.parquet`，列式副本，可选；过滤/排序首屏从约 1.4 s 降到约 0.26 s，源文件变了即作废 |

三条不变量（都有测试）：

1. **原始文件不可变** —— 哈希、大小、修改时间在每条写命令（成功与失败）前后都一样。
2. **成本跟着窗口/操作数走，不跟行数走** —— 一屏 100 行只读 100 行。
3. **会骗人的地方不静默** —— 缺失写 `#N/A`，非数字写 `#VALUE!`，忽略编辑会明说。

三条不变量：

1. **原始文件不可变**——所有编辑都在日志里，导出才物化。
2. **成本跟着窗口/操作数走，不跟行数走**——一屏 100 行只读 100 行。
3. **会骗人的地方不静默**——缺失写 `#N/A`，非数字写 `#VALUE!`，忽略编辑会明说。

## 读取与导航

| 能力 | CLI | API |
|---|---|---|
| 预览（可跳任意行） | `preview f.csv --rows 20 [--start-row N]` | `GET /rows?path=&start=&count=` |
| 建索引 | `index f.csv` | 自动（首次跳转时） |
| 建列式缓存（可选） | `cache f.csv --build` / `--status` / `--drop` | `POST /parquet?path=`（job），状态见 `/sheet` 的 `parquet` |
| 文件元信息 | — | `GET /meta?path=` |
| 机器可读契约 | — | `GET /spec`（端点/参数/字段/错误码）|
| 可观测 | — | `GET /metrics`（p50/p95/状态码/路由）· 每个响应带 `X-Response-Time` |
| 取消长任务 | — | `POST /job?action=cancel&id=`（只有 `cancellable:true` 的才真的停）|
| 健康检查 | — | `GET /health`（存活 + 就绪）|
| 工作簿列表 | `book <dir>` | `GET /book` |
| 表结构 | `preview` / `analyze` | `GET /sheet` |
| 列统计（采样） | `stats f.csv` | `GET /stats?path=` |
| 全列画像 | `analyze f.csv [--sample N]` | `GET /analyze?path=` |

## 编码

| 支持 | 说明 |
|---|---|
| UTF-8 / UTF-8 BOM | 原生 |
| GBK / GB18030 | 探测（能干净解码且含 CJK 才认） |
| UTF-16 LE/BE | 读取；索引拒绝（字节偏移不可靠） |
| Windows-1252 | 与 GBK 打分竞争，`café` 不乱码 |
| 转码 | `convert f.csv -o utf8.csv`（流式，恒定内存） |
| 格式互操作（xlsx 除外） | `convert f.csv --format tsv\|ndjson\|parquet -o out`；反向 `--format csv`；DuckDB 解析/写出，round-trip 测试覆盖引号字段与 Unicode；**xlsx 仍禁用** |
| 引号风格 | `--quotes rfc4180\|backslash\|none`：`preview` `filter` `view` `groupby` `pivot` `chart` `dedupe` `stats` `analyze` `lookup` `search` `columns` `edit export`。`sort` / `concat` / `sql` **还不接受**（按 RFC 4180 读）。检测到反斜杠转义时会自动提示 |

## 分析

| 能力 | CLI | API | 边界 |
|---|---|---|---|
| 分组聚合 | `groupby --by K --agg c:op [--apply-edits] [--engine auto\|native\|sql]` | `GET /groupby` | `--max-groups` 默认 100 万；`--engine auto` 在输入 ≥8 MB 时走 DuckDB |
| 透视表 | `pivot --rows R --cols C --values c:op [--engine auto\|native\|sql]` | `GET /pivot` | 值聚合支持 count/sum/mean(avg)/min/max 与多值列；`--max-columns` 512 / `--max-cells` 200 万；写文件时 `--engine auto` 在 ≥8 MB 走 DuckDB，终端矩阵始终原生 |
| 图表数据 | `chart --x C --y c:op [--series S] [--engine auto\|native\|sql]` | `GET /chart` | `--max-points` 5000；两引擎数值相等，类别顺序可能不同（原生保留首次出现顺序）|
| SQL | `sql f.csv "SELECT ..."` | `GET /query`、`/rows?sql=` | DuckDB，只读单语句，结果封顶 |
| 筛选 | `filter --where "c=v"` | `GET /filter`、`/rows?where=` | 支持 `= != > >= < <= ~ in empty` |
| 搜索 | `search f.csv "text" [--regex]` | `GET /search?regex=1` | 返回行号；`--regex` 用条件格式那套模式子集 |
| 跨表查找 | `lookup --key C --from R --get cols` | `POST /lookup`（job）| `--max-keys` 默认 500 万（公式里的 `ref()` 默认 200 万） `--engine auto\|native\|sql`，`--key a,b` 支持复合键 |

`groupby` / `pivot` / `chart` 读的是**文件**，所以公式列（只在 journal 里）对它们不可见；
加 `--apply-edits` 会先把表物化到临时文件再分析，让「加公式列 → 按它汇总」一步完成。

校验的检查类型：`not-empty` / `numeric` / `int` / `date` / `unique` / 任意比较（`qty>=0`、`city in a,b`）。
`validate` **只读不写**：违规行数和每类计数是精确的，违规明细最多 1000 条（超了会明说），
`unique` 记得住 500 万个不同值（超了也会明说）。

聚合的算子统一是 `count / sum / mean / min / max`；`sum`/`mean` 用 Kahan 补偿求和。

## 编辑（不重写原文件）

| 能力 | CLI | API |
|---|---|---|
| 改单元格 | `edit set --row N --col C --value V` | `POST /edit?action=set` |
| 插入行 | `edit insert --row N --value "a,b,c"` / `--after-key b:12` / `--before-key v:3` | `action=insert` / `insert-after-key` / `insert-before-key` |
| 删除行 | `edit delete --row N --count K` | `action=delete` |
| 按条件删除 | `edit delete --where "city=Puebla"`（一次操作，一次撤销） | `action=delete-where` |
| 按内容定位后修改 | `edit set --find Puebla --col city --value "..."` | — |
| 区间填充（跟着行走） | `edit fill --col qty --from-row 5 --to-row 10 --value 0` / `--cols a,b`（多列一次操作）| `action=fill` |
| 一列拆成多列 | `edit split-column f.csv --col full --by "," --into "first,last,addr"` | `POST /edit?action=split-column&col=&by=&into=` |
| 分页浏览（筛选/排序都看编辑后的值） | `view f.csv [--where EXPR] [--order COL[--desc]] [--find TEXT [--regex] [--nth N] [--context N]] [--start N] [--count M] [--json]` | `GET /rows?where=` |
| 查找替换 | `edit replace --from X --to Y [--col C]` | `action=replace` |
| 删列/改名 | `edit drop --name C` / `edit rename` | `action=drop/rename` |
| 公式列（新增） | `edit formula --name N --value "EXPR"` | `POST /edit?action=formula&name=&value=` |
| 公式列（**重算已有列**） | `edit set-formula --col C --value "EXPR"` | `POST /edit?action=set-formula&col=&value=`（自引用/未知列写入前拒绝）|
| 原子批量写（失败全不写；一次撤销/重做整批） | `edit transaction --ops 'set:1:city:X;delete:2:1;insert:4:a,b'` | `POST /edit?action=transaction&ops=` |
| 撤销 | `edit undo` | `POST /edit?action=undo` |
| 日志压实 | `edit compact`（保留 `.base`/history，跨越边界仍可 undo/redo；10M 条 history 压实 8.98 s，之后 0.02 s；真实 ENOSPC 不改文件） | 暂无 HTTP 动作 |
| 查看待生效 | `edit status` | `GET /edit` |
| 物化 | `edit export -o out.csv` | `POST /edit?action=export` |

编辑是**粘在行上**的：先改第 50 万行、再在第 7 行插一行，那条改动会跟着原行走。
**区间填充也一样**：填第 6–8 行、再删掉前 3 行，值留在这三行上，不会跟着位置滑上去
（1.5.0 之前它按位置存，会滑）。一份连续的行存成区间（填 100 万行也只是一条记录），
跨插入行的填充存成它碰到的行身份列表。
同理**粘在列上**：改名会让已做的改动和公式跟着新列名走，删列则把那一列的改动一起带走。

### 公式

```
算术 + - * / %  ()      比较 = <> > >= < <=
逻辑 if(a,b,c) and or not isblank isnumber istext iserror isna iferror exact
文本 & "字符串" left right mid trim replace substitute find search proper rept
     concat concatenate textjoin char code upper lower len value
数字 round roundup rounddown abs floor ceil int sqrt mod min max sign power exp ln log
     sumsq(A2:C2) product(A2:C2)        ← 同行区间的平方和/乘积（文本忽略，全无数字报错）
日期 year(d) month(d) day(d) date(y,m,d) datestring(d)  ── ISO 前缀（2024-03-15，带时间也行）
     edate(d,n) eomonth(d,n) weekday(d) weeknum(d) networkdays(a,b) workday(d,n) days(end,start)  ── 加减月/月末/星期几(周日=1)/第几周/工作日数/工作日推进/日差；仍返回 ISO 文本
其他 row lag(col[,n]) running_total(col)
整列 sum(col) average(col) count(col)   ← 整列聚合：预扫一遍算出数字列的总和/计数
     min(col) max(col)                 ← 整列最小/最大（文本与空单元格忽略）
     stdev(col) var(col)               ← 样本标准差/方差（Excel STDEV.S/VAR.S）
     stdevp(col) varp(col)             ← 总体标准差/方差（Excel STDEV.P/VAR.P）
     median(A2:C2)                     ← 同行区间的中位数（整列中位数需保留全部值，未做）
     large(A2:C2, 1) small(A2:C2, 1)  ← 同行区间第 k 大/小（k 从 1 起，文本忽略）
     percentile(A2:C2, 0.5)           ← 同行区间分位数（0..1 线性插值，Excel PERCENTILE.INC）
     mode(A2:C2)                       ← 同行区间众数（并列取最小值；无重复值报 #VALUE!）
区间 sum(B2:C2) count(B2:C2)         ← 同行区间：只读本行这两列之间的数字单元格（跨行会让流式求值看到未来行，返回 #VALUE!）
条件区间 countif(B2:C2, ">5") sumif(B2:C2, ">5") averageif(B2:C2, ">5")  ← 条件字面量在解析时编译一次
加权汇总 sumproduct(B2:C2, D2:E2, ...)      ← 同行多段逐项乘加（文本当 0，宽度不等报错）
条件 countif(col, ">1000") sumif(col, ">1000") averageif(col, ">1000")  ← 同一遍预扫；文本条件支持 Excel 通配符 * 与 ?（大小写不敏感）
多条件 countifs(col, ">5", other, "x") sumifs(sum_col, col, ">5", other, "x")  ← 全部条件同时满足才计数/求和
行内多条件 sumifs(A2:C2, D2:F2, ">0", D2:F2, "<3")  ← 同行区间按位置配对（宽度必须一致）
     minifs(max_col, ...) maxifs(max_col, ...)  ← 满足全部条件的行里取最小/最大
     averageifs(val_col, ...)                  ← 满足全部条件的行的平均值（sum/count）
空单元格 countblank(col) countblank(B2:C2)  ← 空单元格数：整列或同行区间
列引用 amount   [客户编号]   [city2]   ← 名字含空格**或以数字结尾**时要加方括号
跨表 ref/vlookup 用本行键取目标表的值；同一行可同时引用多张表；xlookup 多一个 fallback（字面量或列引用），未命中返回它而不是 #N/A
```

### 声明式 book 清单（阶段 3）

`book --plan book.csv --root .` 按 CSV 清单批量物化输出；清单只有三种行，按名字引用，
未知表/未知 lookup/重复名都是硬错误，不会写半个输出。示例：

```text
kind,name,table,path,key,reference,reference_key,get,lookups
sheet,orders,,orders.csv,,,,,
sheet,customers,,customers.csv,,,,,
lookup,customer,orders,,cust_no,customers,cust_no,name,
output,joined,orders,joined.csv,,,,,"customer,region"
```

- `sheet`：名字 → 路径（相对清单目录；`--root` 可覆盖根）。
- `lookup`：从 `table` 用 `key` 查 `reference` 的 `reference_key`，取 `get` 列。
- `output`：从 `table` 起按 `lookups` 顺序链式查找，最后一个写出 `path`；重跑覆盖同一路径，
  因而刷新是幂等的。缺键返回 `#N/A`，与 `lookup` 一致。
- 不做 `Sheet!A1`：输出是物化文件，不是跨表按行随机引用。
- 2.1 GB / 5500 万行、两条链式 lookup：输出 3.1 GB，46–48 s，RSS 15 MB；重跑逐字节一致。

### 质量报告（阶段 4）

`analyze FILE --quality [--key col] [--json | --markdown]` 将逐列画像升级为可执行报告：

- 候选键：非空且 distinct 未截断、distinct == 非空值的列。
- 键检查：`--key key` 报重复值与空键（distinct 截断时明确写“≥”）。
- 类型冲突：同列既有数字又有非数字时 `mixedTypes=true`，并列出数量。
- 数值离群：均值和总体标准差，按 reservoir 估计 mean ± 3σ 之外的数量。
- 分组质量：`--group col` 报组数、空组、最大组占比；单组或占比 ≥80% 告警，distinct 截断时标下界。
- 空值率与 ragged 行数；JSON 含 `quality.candidateKeys/checks/groups/findings`，Markdown 输出表格 + findings。

2.03 GB / 5600 万行 drill（2026-11-23）：analyze --quality **18.71 s**，RSS **17 MB**；
`key` 检查报 55,999,000 个重复值，`city` 报 5,600,000 个空值；候选键为空（id 的 distinct 被 cap 截断）。

### 压缩输入（顺序流）

`search` / `filter` / `analyze` 可直接读 `.zst/.zstd/.gz/.gzip/.bz2/.xz`，解压流在内部完成；
delimiter 检测也读解压后的内容。压缩流不可 seek，因此自动退回顺序路径；`index`/`view`
仍未承诺随机访问。

2.3 GB/9500 万行 drill：plain filter 1.51 s/57 MB；`big.csv.zst`（51 MB）filter **4.16 s/15 MB**，
输出与 plain `cmp` 逐字节一致；zst search 12.13 s/11 MB；zst quality 21.73 s/20 MB。
压缩读是 CPU 受限，不能用它推断 100 GB 指标。

### 100 GB 级（C 主线，已达标）

104.5 GB / 13 亿行（重复块合成）：`preview` 0.02 s/8 MB，`index` 24.57 s/21 MB（索引 9.9 MB），
`search` 20.18–20.42 s/93 MB，`filter` 25.75 s/95 MB（1.3B scanned、2.6 亿 matched），
选择性 `filter -o` 19.54 s/17 MB，深跳第 7 亿行 0.02 s/29 MB。
验收目标 search <60 s、filter（扫描）<90 s、RSS <200 MB 满足；全量物化 `filter -o`
（200M 行/20 GB）用两遍扫描 + 偏移写入做到 **79.62 s / 28 MB**，也进入 90 s。
按输入大小自适应（≥64 GB 两遍，<64 GB 保留 part 并行 copy），避免中等文件回归；
详见 `docs/BENCHMARKS.md`。

### 81 GB 真实字段演练

7 列、11.6 亿行、87.4 GB：search 39.42 s、filter 46.70 s、全量 17% 导出
196,928,881 行/13 GB **62.03 s / 28 MB**；仍为 21.8 GB 块重复 4 次。

### 离线发布包

`bash scripts/package_release.sh` 生成带 SHA256 的 `dist/csvx-<version>-<platform>.tar.gz`；
含二进制、README、LICENSE、主要文档与 demo/pilot 脚本。用于企业内部分发/试点，不打 tag。

### 试点报告

`CSVX_PILOT_FILE=sample.csv python3 scripts/pilot_report.py`
对客户样本跑 schema/filter/search/quality，产出 Markdown 报告；默认本地、无遥测。
见 `docs/PILOT.md`。

### 端到端 demo

`BIN=target/release/csvx bash scripts/demo_end_to_end.sh`：pipeline filter→quality（12/11 ms）
后，`scripts/mcp_client.py` 走 schema/filter/view/edit/undo/convert/job，16 个工具。
见 `docs/DEMO.md`。

### MCP（B agent 后端，只读首版）

`mcp --root DIR` 在 stdio 上跑 JSON-RPC MCP server，stdout 只走协议，工具只读：

- `csvx_schema`：列、类型、行数。
- `csvx_view`：`start/count` 分页、可选 `where`；JSON 返回 `start/nextStart/hasMore`
  游标，客户端用 `nextStart` 取下一页。
- `csvx_filter`：谓词匹配行号，`limit` 上限 1000。
- `csvx_search`：文本搜索，可按列。
- `csvx_quality`：`analyze --quality` 报告，支持 key/group。
- `csvx_pipeline_status`：读 `.pipeline.csv` 的哈希与耗时。
- `csvx_pipeline_run`：直接执行声明式 pipeline（`force`/`dryRun`/`dag`/`jobs`/`clean`），
  agent 可触发拓扑 DAG、并发上限与 staging 清理；长任务可再包 `csvx_job_submit`。

路径必须落在 `--root` 内；写工具复用 journal 锁：
`csvx_edit_set`（row/col/value/expect）、`csvx_edit_delete`（row 或 where）、`csvx_edit_undo`、`csvx_edit_redo`、`csvx_convert`（tsv/ndjson/parquet/csv，输出限制在 root）。
长任务可走异步 job：`csvx_job_submit`（tool + arguments）返回 `job-1`，`csvx_job_status` 返回
running/done（带 output）/failed（带 exitCode/error）；结果会缓存，重复轮询不丢输出。
`initialize` / `tools/list` / `tools/call` 已实现；UTF-8 参数按字节保留，JSON-RPC 一行一条。

官方 `@modelcontextprotocol/sdk` 1.32.2 客户端联调通过：13 工具、schema/filter/view、
edit/undo/convert、异步 job 均完成（见 `docs/DEMO.md`）。
2.3 GB/9500 万行 MCP drill（2026-11-23）：cold `schema` 3.11 s（建 760 KB 稀疏索引），
warm schema 0.03 s，`view start=45,000,000` 0.02 s，`filter` 0.77 s，`search` 0.56 s，
`quality` 22.25 s（9500 万行全量）；响应 0.4–4 KB。

### Pipeline（A 工作台，进行中）

`pipeline run plan.csv --root .` 按 CSV 清单顺序执行步骤，复用现有命令：

```text
step,input,command,args,output[,group]
filtered,orders.csv,filter, -o {out},filtered.csv
joined,filtered,lookup,--key cust_no --reference customers.csv --get name --out {out},joined.csv
```

- `--dag --jobs N`：按依赖拓扑调度（空输入依赖上一步、步骤名依赖更早步骤、文件路径无依赖）；
  每波只启动 stale 就绪步骤，fresh 步骤复用 state；失败不写主 state。`group` 列在 DAG 模式被忽略。
- 可选第 6 列 `group`：同一 group 的**连续**步骤在 `--jobs N`（N>1）下并发执行；
  组内步骤必须显式指定 input（源文件或更早步骤输出），不能互相依赖。组内逐步骤判断
  fresh：fresh 成员直接复用上轮 state，只把 stale 成员交给并发子进程；任一兄弟姐妹失败时，本波其余子进程被 kill，本波 staging 文件清理；最终输出
  只在整波成功后 rename，因此失败不会破坏上一版输出；主 `.pipeline.csv` 不写；
  更早成功波已提交的输出仍保留。
- `--clean` 在运行前清理本计划输出对应的 staging/legacy 临时文件；普通运行只自动清理
  超过 1 小时的残留，`--clean` 强制立即清理。
- `--step-timeout <ms>` 对每个步骤做硬超时：kill 子进程、删除临时输出、不写 state；
  `0` 表示不限；stdout/stderr 由独立线程并发读取，避免管道写满死锁。
- `--retries <n>` 与 `--retry-delay <ms>`：失败/超时后重试，默认 0 次、250 ms；
  每次重试先删除临时输出；命令必须可重复写 `{out}`，否则不要开启重试。
- 同一计划有 `<plan>.pipeline.lock` 运行锁：已存在且未超过 24 小时会拒绝第二次运行；
超过 24 小时的锁视为残留并自动移除。锁在正常返回/失败时释放。
`--unlock` 可在运行前显式删除锁；仅在确认没有其他 run 时使用。
`pipeline status --verify` 按当前 input/output 的 stamp/hash 重新判断 fresh/stale；
JSON 增加 `"fresh"`。成功运行后会写 `<plan>.lineage.csv`（列 `step,deps`，deps 以 `;` 分隔），dry-run 不写。
`--dry-run` 只报告 fresh / would-run，不执行、不写 state；`--json` 输出
  `{"dryRun":true,"steps":[{"step","fresh","detail","deps":[...]}]}`，`deps` 是声明式
  血缘（空输入依赖上一步，步骤名依赖更早步骤，文件路径无步骤依赖）。
- `input` 为空表示上一步输出；也可写前一步的 `step` 名或文件路径。
- `{out}` 会被替换为原子临时文件，成功后才 rename；不带 `{out}` 时捕获 stdout 写成输出。
- `.pipeline.csv` 记录输入/参数/输出哈希；未变步骤显示 `↷` 跳过，变了的重跑，`--force` 全跑。
- 失败不写状态、不留 partial 输出；`pipeline status --json` 查看血缘哈希与每步耗时。
- 2.3 GB/9500 万行 drill（2026-11-23）：filter→lookup→groupby 总 **6.55 s**；filter 0.65 s/29 MB，
  SQL lookup 0.70 s/244 MB（1900 万/1900 万命中、0 missing），groupby 0.40 s/194 MB；
  增量重跑 **17 ms**（size+mtime 快路径，不再读文件算哈希）。

`if` 只算选中的分支（`if(qty=0, 0, amount/qty)` 不会除零）。

整列聚合（`sum`/`average`/`count`）在公式开始前扫一遍**基文件**，把数字列的
（总和, 计数）装进状态；之后每行是 O(1) 查表。代价是 100 万行约 0.48 s（
`edit export` 0.85 s → 1.33 s），有列式侧车（`cache --build`）时走 DuckDB parquet
快路降到约 0.30 s。结果只依赖基文件字节，所以会写一份 `t.csv.agg` 侧车（按文件长度
与 mtime 失效）：**同一文件重复导出不再重扫**——100 万行 0.15 → **0.09 s**。预扫本身：重复调用约 30 ms，冷启动约 50–60 ms（小文件走列切片+并行，大文件仍可用 parquet）。
2.1 GB 6.65 → **4.56 s**。
也明确不支持区间（`sum(a1:a9)`），且只读文件、不包含 journal 里尚未导出的编辑。
空单元格当 0；非数字得 `#VALUE!`；引用不存在的列 → **导出前报错**。

公式列的开销分两种。**每行相同**的公式（整列 `sum`/`average`/`count`、`countif` 等）不必逐行渲染：
导出把原行字节原样复制、只在末尾追加那一格，100 万行 **0.13–0.33 s**（含 DuckDB 启动差异），2.1 GB / 5000 万行 **7.2 s**
（优化前 70 s；预扫改成多线程后从 15.4 s 再降，同一文件不带公式 2.9 s）。**要读当前行**的公式（`lag`、`amount*2`、
`if(...)`）仍必须逐行重写，那才是要避免在大文件现场导出的情形。

## 格式化与呈现

| 能力 | CLI | API |
|---|---|---|
| 条件格式 | `format --rule "amount>1000:bg=red" [--summary]` | `GET /style?rules=...` |
| 列宽/显示名/冻结 | `layout --set "c:width=160"` | `GET /layout` |
| 数字格式 | `--set "c:format=#,##0.00"` | 同上 |
| 内置浏览页面 | — | `GET /` |

条件格式的规则语法：`列<运算符>值:样式`，运算符 `> >= < <= = != ~ ^ $ matches between empty nonempty`，
样式 `bg= fg= bold italic`（颜色用名字或 `#rgb`/`#rrggbb`）。**后面的规则赢**。

## 交付

| 能力 | CLI | API |
|---|---|---|
| 切分 | `split f.csv -n N` / `--by-size` | `POST /split` |
| 合并 | `merge chunks.tar -o out.csv` | `POST /merge` |
| 排序 | `sort --by C -o out.csv` | `POST /sort` |
| 列集合对比 | `schema a.csv --diff b.csv`（新增/缺失/移位，退出码 1）| `GET /schema?diff=` （同一份 JSON）|
| 列过滤/截断 | — | `/columns?kind=number&limit=20`（带 totalColumns）|
| 撤销栈查询 | `edit status` | `GET /journal`（含 redo 栈与损坏提示）|
| 按 key 定位行 | `/rows?keys=b:12,v:3`（插入/删除后仍指向同一行）| — |
| 并发写入 | 两个进程同时 `edit` | journal 加锁，双方操作都保留（实测 10 并发 → 10 条）|
| 窗口选列 | `/rows?columns=a,b`（782 列 → 3 列，87 KB → 529 字节）| — |
| 列存在性 / 列元数据 | `schema --has COL`（存在 0 / 不存在 1）· `schema --json` | `GET /columns` |
| 写入约束 | `edit set --expect VALUE`（不匹配则拒绝）| `POST /edit?action=set&expect=`（409）|
| 数据校验 | `validate --check "qty>=0" --check "city:not-empty"` | `GET /validate?checks=a;b[&limit=N]`（全量是整文件扫描，`limit` 给前缀检查）|
| 修复 journal | `repair f.csv [--dry-run]` | — | 先备份，再只保留可读记录；`--dry-run` 只报会丢几条 |
| 删掉不合格的行 | `edit delete --invalid "ts:date"`（一次操作，一次撤销） | — |
| 去重 | `dedupe -o out.csv [--by C]` | `POST /dedupe` |
| 分文件 | `partition --by C -o parts.tar` | `POST /partition` |
| 拼接 | `concat a.csv b.csv ...` | `POST /concat`（job） `--engine auto\|native\|sql`；SQL 走 `COPY`，表头仍先原生校验 |
| 导列/改名 | `columns --keep/--drop/--rename` | `POST /columns?keep=&drop=&rename=`（job，写新文件）|
| 下载/导出 | — | `GET /download`、`/export` |
| 起本地服务 | `serve --root DIR` | 全部端点 |
| 列出可读文件 | `files --root DIR` | `GET /files` |
| 生成示例 | `demo [-o DIR]` | — |

切分/合并是**字节级无损**（`verify` 逐字节比对）；其他写操作支持 `--verify` 计数级自检。

## 内存与边界（实测）

| 操作 | 内存 | 说明 |
|---|---|---|
| 跳转任意行（9.9 GB） | 11.3 MB | 稀疏索引 |
| `analyze` / `preview`（322 MB） | 17.1 / 8.3 MB | 2026-11-10 复测 |
| 全列 analyze（1 GB） | 29 MB | 流式 |
| 切分（9.9 GB） | 89 MB | 输出缓冲 |
| 合并（10.7 GB） | 10.7 MB | 流式合并 |
| 编辑 100 万行 | 60 字节日志 | 只有操作数占空间 |
| 透视 100 万行 | 矩阵大小 | 5×12 = 60 个累加器 |

**全命令实测**（release 版，77 MB / 400 万行的文件，`/usr/bin/time -l` 的峰值 RSS）：

| 命令 | 峰值内存 | 为什么是这个数 |
|---|---|---|
| `preview` / `search` / `chart` / `pivot` / `groupby` | **8–9 MB** | 流式 |
| `filter -o` / `columns` / `validate` | 12–13 MB | 流式 |
| `index` | 16 MB | 稀疏索引（1/7000 文件大小）|
| `analyze` / `stats` | 23–25 MB | 采样 + 每列基数上限 |
| `split -n 4` | 64 MB | 输出缓冲 |
| `verify` | **44 MB** | 逐块比较（曾经把两个文件整个读进来，见 1.9.0）|
| `sort` | 218 MB | **排序缓冲**（内存预算参数，默认 256 MB）|
| `partition` | 287 MB | 同上：先按分区列排序 |
| `dedupe`（4M 个不同键） | 285 MB | **记得住的键**（`--max-keys` 上限 1 亿）|
| `dedupe --by city`（20 个键） | **9 MB** | 键少，所以省 |

## 大文件过滤与搜索（2 GB 实测）

`filter` 与 `search` 都要看完整张表，所以它们的成本就是「一遍全表」。并行化不改语义，只改
墙钟时间：UTF-8 文件按稀疏索引的检查点切成每核一段（封顶 8 段），每段各自 seek、各自解析，
最后按行号合并。GBK / UTF-16 的字节偏移不是行边界，仍走顺序扫描。

| 场景 | 数据量 | 并行 | 顺序 | 峰值内存 |
|---|---|---|---|---|
| `filter --where city=Tijuana`（1250 万命中） | 2.1 GB / 5000 万行 | **2.7 s** | 13.5 s | 43 MB |
| `search`（无命中） | 2.1 GB / 5000 万行 | **4.8 s** | 18.1 s | 42 MB |
| `GET /filter` / `GET /search` | 2.1 GB | 2.1 / 3.1 s | — | — |

行号仍与 `/rows` 对齐：`filter_can_apply_edits_and_its_row_numbers_follow` 与
`parallel_scan_agrees_with_sequential` 钉住这两件事。30 秒的预算仍在：UTF-8 并行路径会把
预算内已完成的一页返回，并给出 `next` 游标；只有顺序扫描路径（GBK/UTF-16 或小文件）
在超预算时返回 504。

同一份测量放大到 11.2 GB / 2.5 亿行（`scripts/drill.sh` 输出）：

| 场景 | 数据量 | 并行 | 峰值内存 |
|---|---|---|---|
| `filter`（6250 万命中） | 11.2 GB / 2.5 亿行 | **13.2–13.9 s** | 79 MB |
| `search`（无命中） | 11.2 GB / 2.5 亿行 | **13.8–14.6 s** | 78 MB |
| `GET /filter` / `GET /search` | 11.2 GB | 10–16 / 14–20 s（随负载波动） | — |
| 打开 / 跳到第 1.2 亿行 | 11.2 GB | 0.01 / 0.01 s | 8 MB |
| 编辑一格 / 导出 2.1 GB / `verify` | 2.1 GB | 0.01 / 1.9 / 7.0 s | 8–46 MB |

**耗时随行数而不是字节数增长**（2026-11-23 复测）：同一 11 GB 体积，2.5 亿行时过滤/搜索
13.2–14.6 s；2.8 亿行时过滤 **14.9 s**、搜索 **16.3 s**（行数 +12%，时间 +8–18%，内存仍 78–81 MB）。
用字节数估算会把宽行文件估得过于悲观；择容量时先看行数。

### 阶段 1：读路径 profile 与冻结验收（2026-11-23）

夹具 1 GB / 2000 万行（与 drill 同构的列），release 二进制，三次中位数，页缓存已热；
线程数用 CSVX_SCAN_THREADS 控制。filter 的 -o 写文件路径与无输出路径是两条不同实现。

| 路径 | 1 线程 | 4 线程 | 8 线程 |
|---|---|---|---|
| filter -o（无命中，只写表头） | 4.35 s | 4.09 s | 4.23 s |
| filter 无输出（无命中） | 4.51 s | 1.66 s | 1.17 s |
| search（无命中） | 4.54 s | 2.13 s | 1.26 s |
| wc -l（纯 I/O 下限，仅参考） | 0.51 s | — | — |

结论（sample 栈）：

- 两条路径的热点都在 parse_csv_record_with：每行构造字段时反复 RawVec 增长/realloc；
  filter -o 走 extract_style，完全串行，所以线程数不改变 4 s 级耗时。
- 无输出 filter 与 search 会并行，1 GB 已到约 1.2 s/GB；剩余成本主要仍是解析分配。
- 1 GB 的 4 s 与 11 GB 的 14.9 s 不能直接按体积比：大夹具已有索引、行长与命中数不同；
  阶段 1 的验收统一用「同构 11 GB / 2.5–2.8 亿行」与同线程数。

**冻结验收（阶段 1 完成条件）**

| 验收项 | 当前基线 | 目标 |
|---|---|---|
| filter -o（11 GB / 2.8 亿行） | 14.9 s | ≤ 8 s |
| filter 无输出（11 GB） | 14.9 s | ≤ 10 s |
| search（11 GB） | 16.3 s | ≤ 11 s |
| search 30 s 预算边界 | 约 20–22 GB | ≥ 30 GB |
| 以上路径峰值 RSS | 77–81 MB | ≤ 90 MB |
| 语义 | — | filter -o 输出与改动前逐字节一致；全量 gate 绿 |

先改 filter -o 的串行/分配，再评估无输出与 search；任何一项测不出收益就回退，不靠放宽断言。

三轮优化（2026-11-23）：extract_style 对无引号记录改用借用字段匹配并直接写回源记录字节，
不再每行构造字段 String 或 clone 行；再用 visit_fields 边扫边判定，去掉每行 field_slices 的
Vec 分配；最后对 UTF-8 文件按行边界分片，worker 写有序 part，主线程合并。1 GB 的 filter -o
从 4.31–4.81 s 降到 **0.15–0.19 s**（8 线程）。

同构 11 GB / 2.8 亿行验收（2026-11-23，8 线程，release）：

| 场景 | 耗时 | 峰值 RSS | 冻结目标 |
|---|---|---|---|
| filter -o 无命中 | **2.61 s** | 17 MB | ≤8 s ✅ |
| filter -o Tijuana（5600 万命中、2.2 GB 输出） | **5.26 s** | 28 MB | ≤8 s ✅ |
| filter 无输出（无命中） | **3.58 s** | 77 MB | ≤10 s ✅ |
| search（无命中） | **2.62 s** | 77 MB | ≤11 s ✅ |

filter -o 输出 56,000,001 行（含表头），首行 id=2、末行 id=279,999,997，无重复/缺口；分片临时文件
全部清理。search 命中 56,000,000 行，唯一 id 查询返回准确的 1-based 行号，跨分隔符查询 0 命中——
说明字节谓词没有越过单元格边界。

31 GB / 7.8 亿行边界验收（2026-11-23，8 线程，release）：建索引 11.11 s；search 无命中 **7.38 s**；
filter 无输出 **10.13 s**；filter -o 无命中 **7.81 s**；RSS **81 MB**。全部在 30 s 内，
**30 s 预算边界 ≥30 GB 达成**。同一文件首屏 0.02 s、跳到第 3.9 亿行 0.02 s。

阶段 1 完成：11 GB 的四项冻结目标与 30 GB 预算边界都有实测证据；下一步进入阶段 2（journal 压实与崩溃恢复）。

**边界**：搜索在 11.2 GB 上已用掉约 15 s，30 s 预算的实际上限约 20–22 GB；更大的文件
不必 504——把 `/filter` 或 `/search` 返回的 `next` 作为 `cursor` 再请求即可续扫，
行号仍与 `/rows` 对齐。

所以「内存与行数无关」的准确版本是：**与行数无关，除非那个命令必须记住每一行**
（`dedupe` 要记住见过的键；`sort`/`partition` 要一片缓冲区）。这三种都有显式上限：
`--max-keys` / 内存预算参数，超了会报错而不是悄悄 OOM。

## 明确的非目标

- 不做公式图表/透视表的**图形渲染**（只提供数据，前端画）。
- 不做多人协作、不做事务。
- 不修改原文件、不产生隐式备份。
- 不做 xlsx 读写。

## 已知短板

| 短板 | 影响 |
|---|---|
| 内置页面没在真浏览器验证过 | 有契约测试兜底，布局未经人眼 |
| `analyze` 全表扫描（1 GB 约 11 s） | 用 `--sample N` 可以快 5 倍，但那只是**前 N 行的估算** |
| Windows 未实测 | 路径处理可能有坑 |
| 多 sheet 工作簿只有列表，没有跨表引用 | 还不能 `=customers!B2` |



## 断言核对 / Claims checked against the code

文档里的每一条「支持 / 不支持」都对着代码和实际命令验过（2026-11-10）：

| 断言 | 结果 |
|---|---|
| UTF-16 文件拒绝建索引（字节偏移不可靠） | ✅ 有明确提示并给出 `convert` 建议 |
| SQL 只允许单语句 | ✅ `SELECT 1; SELECT 2` → `only one statement is allowed` |
| SQL 只读 | ✅ `DROP TABLE t` → `only SELECT / WITH queries are allowed` |
| 筛选算子 `= != > >= < <= ~ in empty` | ✅ 逐个跑过 |
| 透视 `--max-columns` 512 / `--max-cells` 200 万 | ✅ 与 `DEFAULT_*` 常量一致 |
| 分组 `--max-groups` 100 万 | ✅ |
| 图表 `--max-points` 5000 | ✅ |
| `book` 最多 500 张表 | ✅ |
| `lookup --max-keys` 500 万 | ✅（此处原写「500 万」，`ref()` 是 200 万，已注明） |
| `--quotes` 所有读命令都支持 | ❌ **原文档写错了**：`sort`/`concat`/`sql` 仍不接受。`columns` 和 `search` 已在 0.88.0 补上，文档改成逐个点名 |

最后一条是这次核对唯一的错误，也是最容易造成静默错误的：文档说「所有」，
用户就会以为 `sort --quotes backslash` 可用，实际会按 RFC 解析。
现在有一个测试逐条跑这些命令，确认它们真的解析了这个参数。


## 派生值的语义 / What derived values read

一条规则，钉在测试里：**任何派生值都读「当前这一行」的值，包括编辑过的。**
这条规则正好是前几轮出 bug 的地方，所以写在这里而不是留在代码注释里。

| 派生 | 读什么 | 验证 |
|---|---|---|
| 公式列 `qty*2` | 同一行的 `qty`，**编辑后** | ✅ |
| `lag(qty)` | 上一行的完成态（编辑后） | ✅ |
| `running_total(qty)` | 逐行累积，含编辑 | ✅ |
| 分列片 | **base 文本**（若那一片被显式编辑，则以编辑值覆盖该片） | ✅ |
| 条件格式 | 完成后的行 | ✅ |
| 公式读分列片 | 该片的当前值 | ✅ |

两条容易搞错的：

- **分列片从 base 派生**，不是从「可能被改过的源单元格」派生——否则改第一片会把
  其余片清空（0.90.0 修的正是这个）。
- **改名会把已有的编辑和公式一起带到新列名**（0.89.0）。列身份和行身份同等重要。


## 列类型是采样推断 / Column kinds are a sample, not a promise

`schema` 和 `/columns` 报告的 `kind`（`number` / `date` / `text` / `empty` / `mixed`）只
看前 `--sample` 行（默认 10）。它的用途是让前端选一个编辑器，不是保证——第 11 行起完全
可能是别的类型。要判断整列，用 `analyze`（全表一遍）；要断言整列，用 `validate`。

## 真实宽表实测 / A real 782-column file

用一份**真实业务结果文件**（2.5 GB / 68.5 万行 / 782 列）跑过一遍，因为 782 列宽表会
放大任何「每行每列都做点小事」的开销：

| 操作 | 结果 |
|---|---|
| `index` | **0.31 s**，峰值内存 **15.9 MB**，索引 **5.4 KB**（文件大小的 1/500,000）|
| 任意偏移看一屏 | **12 ms**（第 68 万行与第 1 行同价）|
| `filter model_prob>0.5`（33 万行命中）| 13.5 s，输出 1.28 GB |
| `groupby predict_status` | 14.6 s，峰值内存 **9.0 MB** |
| `validate` 两条范围检查 | 61 s |
| `edit export`（无编辑）| 40.7 s（62 MB/s）|
| `edit export`（1 个公式列）| 69 s |
| `preview` | 0.9 s，内存 9 MB |

**后端在这份文件上的实测（release，本机 HTTP，2.5 GB / 782 列 / 684 917 行）**：

| 调用 | 结果 |
|---|---|
| `GET /capabilities` | 1.5 ms，1008 字节 |
| `GET /columns?name=model_prob` | **2.8 ms**，47 字节 |
| `GET /columns`（782 列全量） | 0.67 s，107 KB |
| `GET /columns?kind=number` | 1.6 ms（第二次，已缓存读）|
| `GET /rows?start=680000&count=10`（全列）| **2.4 ms**，88 KB |
| 同窗口 `&columns=…`（3 列）| **0.9 ms**，667 字节 |
| `GET /rows?keys=b:5,b:100` | 1.0 ms，126 字节 |
| `GET /rows?start=999999`（越界）| 0.7 ms，`pastEnd:true` |
| `GET /cell` | **1.6 ms**，80 字节 |
| `GET /journal` | 0.4 ms |
| `GET /rows=` 不过滤首屏（3 列）| **1.1 ms** |
| `GET /rows?where=` 首屏（全命中，3 列，**冷启动无缓存**）| **0.22 s**（原 8–9 s）|
| 同一请求再发一次 | 0.19 s（且**不产生 `.idx` 副作用**：采样不再建行索引）|
| 同上，命中在中段（第 30 万行）| 1.48 s（要扫到那一块）|
| 同上，0 命中（必须扫完整个文件）| 1.36 s（原 15 s）|
| 同上，**建过列式缓存**（`POST /parquet`）| **0.26 s**（0 命中同样 0.2 s 级）|
| `GET /rows?order=` 首屏（冷）| 1.36 s（原 15.5 s）|
| `GET /rows?order=` 首屏，有缓存 | **0.34 s** |
| `GET /rows?order=` 首屏（top-N，3 列）| **2.7 s**（原 15.5 s：不再收集 20 万行）|
| `GET /rows?where=` 0 命中（必须全扫）| 2.9 s（原 15 s）|
| `GET /query?sql=count(*)` | 2.0 s（声明列类型后；原 8.6 s）|
| `GET /query?sql=SELECT *&limit=5` | 8.0 s（同样是整表扫描）|
| `POST /edit?action=set&expect=` 正确 | 200，值立即在 `/cell` 与 `/rows` 可见 |
| 同一请求再发（`expect` 过期）| **409**，`actual` 是刚写入的 `checked` |
| `POST /edit?action=export`（带 where，1 行命中）| job 40 s 完成，文件保留 BOM |

**慢的两种，原因都清楚**：

1. **宽表逐个单元格物化**。`export` / `validate` / `analyze` 走的是「每行物化成
   `Vec<String>`」的路径（782 个 `String`/行），2.5 GB 下约 40–60 MB/s；
   纯字节流过的 `filter` 是 190 MB/s。窄表看不出差别，782 列放大 15 倍。
2. **`validate` 曾经每行每规则重建一次整行探针**（782 个空 `String` × 两个规则 × 68 万行），
   实测 72 s；改成每规则复用一个探针后 61 s——剩下的仍是第 1 条的结构成本。

**已知的改进方向**（未做）：让 `RowPipeline` 用借用的 `&str`（`Cow`）而不是每格一个
`String`，宽表的导出和校验能再快 2–3 倍。这是结构性改动，需要的时候再做。

## 后端自检 / Backend self-check

`scripts/backend-smoke.sh` 起一个真服务、跑 27 项断言（能力/列/窗口/写约束/journal/错误码）。
和 cargo 测试互补：cargo 调 handler，这个调 HTTP。

## 中断与损坏 / Interrupts and damage

长命令被打断是常态，所以每个 sidecar（旁挂文件）都必须能自我修复，且**修复本身不能骗人**：

| 损坏的东西 | 现在的行为 |
|---|---|
| `.journal` 尾部被截断（写到一半掉电）| 读得到的操作照常生效；**读不到的记录会明确警告**「N 条记录读不出来，已忽略」|
| `.journal.redo` 损坏 | 同上，警告里点名是 redo 文件 |
| `.idx` 截断或整块垃圾 | 校验不通过就**重建**；重建后行号正确（测试验证过第 91 行）|
| `.view`（列宽/冻结/格式）损坏 | 回退到默认值，文件照常能打开 |
| 命令中途失败 | 不留半截输出（9 种失败模式实测无残留）|

「读得到的照常生效 + 读不到的明确说出来」是这个文件格式的核心：**journal 是逐行追加的，
所以中断只会损失最后一条，不会损失整个历史。**
## 成本边界 / Where the promise ends

「任意大小不卡」指的是**内存**：任何命令的峰值内存都与行数无关（见下面的实测）。
时间成本有三档，值得说清楚，因为它们是三种完全不同的体验：

| 操作 | 时间成本 | 什么时候付 |
|---|---|---|
| 翻一页（有索引） | **O(页大小)** | 每次 | 
| 首次打开没有索引的文件 | O(文件大小) | **一次**，之后存盘 |
| `where=` / `order=` / `sql=` 第一次 | O(文件大小) | 每个不同的查询一次，结果进缓存 |
| `where=` 翻第二页 | O(页大小) | 缓存命中 |
| `analyze` / `search` / `dedupe` / `sort` | O(文件大小) | 每次（它们的工作就是看全表） |

实测（215 MB / 500 万行，release，本地服务）：

| 请求 | 耗时 |
|---|---|
| `/rows` 首次（含建索引） | 0.066 s |
| `/rows` 之后任意页（含深偏移） | **0.006 s** |
| `/rows?where=…` 首屏（物化） | 0.296 s |
| 同一筛选条件的后续页 | **0.015 s** |

**所以「不卡」的准确含义是**：打开、翻页、看编辑后的表都是常数时间；而需要看全表的
操作（筛选物化、排序、全列画像）每次都要读一遍文件——那是它们的工作量，不是缺陷。

结果缓存的边界：单条结果最多 **20 万行**、总量 **40 万行**，超出就不缓存（不截断、不假装），
文件长度或修改时间变化即失效。

## 大文件拆分 / Module layout

模块职责与「改一处要看哪里」的对照表见 [ARCHITECTURE.md](ARCHITECTURE.md)。


| 文件 | 行数 | 内容 |
|---|---|---|
| `crates/core/src/workbook/mod.rs` | ~1075 | `Op`、`Sheet`、列布局、公开接口 |
| `crates/core/src/workbook/rows.rs` | ~832 | 行身份、逻辑↔物理映射、单元格编辑、公式编译 |
| `crates/core/src/workbook/journal.rs` | ~346 | journal 结构、文本格式、撤销/重做栈 |
| `crates/core/src/workbook/export.rs` | ~379 | 导出、窗口读取、行装配 |

`workbook.rs` 曾经超过 2500 行。拆分的判据不是行数本身，而是**改动的边界**：
导出和窗口读取会接触每一行，是最常变的部分；journal 的文本格式几乎不变。

## 缓存与失效 / Caching

服务端有两处缓存，都按同一个 `Stamp` 判断是否过期：

| 缓存 | 存什么 | 失效条件 |
|---|---|---|
| pager 缓存 | 打开的 sheet（索引、列布局、公式） | journal 内容哈希、基础文件大小/时间戳、分隔符任一变化 |
| 物化缓存 | 编辑后表的临时副本（给 `where=`/`order=` 用） | journal 内容哈希变化 |

**journal 用内容哈希而不是时间戳**：同一次时间戳内写两次，`(mtime, len)` 不变，
缓存会返回上一版——这在上线前真的发生过一次。基础文件不哈希（可能 10 GB），
它本来就该不可变。

实测（20 万行、1 条编辑）：首个请求 41 ms，之后任意页 **6 ms**（含第 15 万行）。

## 输出保护 / Output safety

`-o` 指向**正在读的输入文件**时直接拒绝（exit 1，说明原因）——那是数据丢失，
而且改错一个字符就会发生。指向其他已存在文件时允许覆盖，但会在 stderr 说明
`note: overwriting existing out.csv`，这样交互使用时不会在不知情下清掉旧结果。

覆盖保护覆盖：`filter` / `sort` / `columns` / `dedupe` / `lookup` / `groupby` /
`pivot` / `convert` / `edit export`。

### concat 是唯一的例外：auto 始终走原生

`concat` 的主要工作是**拷贝字节**，不是读取值。原生路径只解析表头、按记录复制；
DuckDB 要解析每一列、写出文件、再扫一遍统计行数。实测：

| 输入 | native | DuckDB |
|---|---|---|
| 18 MB × 2 | **0.07 s** | 0.20 s |
| 90 MB × 2 | **0.26 s** | 0.88 s |
| 89 MB × 2（2026-11-19 复测）| **0.25–0.28 s** | 0.78–0.92 s |

所以 `concat --engine auto` **永远走原生**（`engine::resolve_for_copy`），要 DuckDB 得显式
`--engine sql`——那里没有「文件大了就自动切换」的逻辑，因为没有交叉点可切。

### 三个命令的交叉点一致（实测）

同一个 8 MB 阈值能不能管住所有命令？分别测：

| 输入 | chart native | chart DuckDB | pivot native | pivot DuckDB |
|---|---|---|---|---|
| 0.9 MB | **0.02 s** | 0.07 s | — | — |
| 3.6 MB | **0.03 s** | 0.04 s | **0.05 s** | 0.07 s |
| 9.1 MB | 0.08 s | **0.06 s** | — | — |
| 18.1 MB | 0.14 s | **0.07 s** | 0.19 s | **0.11 s** |

三个命令的交叉点都落在 4–9 MB 之间，所以**一个阈值够用**，不需要按命令各配一套。
pivot 两个引擎的输出在这两个尺寸上逐字节相同。

### auto 阈值怎么定的（实测）

`--engine auto` 的阈值不是拍的：同一台机器上跑 `groupby`（7 组、一个 sum），

| 输入 | native | DuckDB |
|---|---|---|
| 0.9 MB | **0.01 s** | 0.03 s |
| 1.8 MB | **0.02 s** | 0.03 s |
| 3.6 MB | 0.04 s | 0.04 s |
| 9.1 MB | 0.08 s | **0.06 s** |
| 18.1 MB | 0.16 s | **0.07 s** |

交叉点约在 4–9 MB，所以阈值定 **8 MB**。原来的 64 MB 没有数据支撑，中间 7 倍的区间
一直在走更慢的原生路径。

## 契约审计（2026-11-19）/ Contract audit

逐项核对过声明与实现，结论留档，免得下次重新查：

| 端点 | 检查 | 结论 |
|---|---|---|
| /rows | format 是否实现 | 曾**只写在文档**，现已实现（format=csv 输出编辑后的窗口）|
| /rows | keys whereAny | 读取，有测试 |
| /rows | start-col count-cols | 经 slice_columns 读取（跨行写法，grep 直接搜参数名搜不到），有测试 |
| /cell | 是否带稳定 row key | 带（"key":"b:12"）|
| /columns | 声明里混着 GET 与 POST 的参数 | 已知：/spec 按路由而非按方法列参数，keep/drop/rename/out 只对 POST 有意义；GET 收到它们会接受但忽略 |

最后一行是**结构性问题**（/spec 的 params 键是路由，不分方法），修它要改契约格式，超出单轮范围；在那之前，
GET /columns 多带的四个参数属于「声明正确、对该方法无意义」。

## `apply-edits` 支持矩阵 / Which endpoints can apply edits

一条规则：**读表并回答读数的端点支持 `apply-edits=1`；产出文件副本的端点不支持**。

| 端点 | `apply-edits=1` | 备注 |
|---|---|---|
| `/rows` | 总是套用编辑（`sql` 路径也一样）| 渲染路径的默认行为 |
| `/rows?format=csv` | 同上 | 拿到编辑后文本的唯一途径（`/export` 默认读基文件）|
| `/cell` `/style` `/journal` `/sheet` `/columns` `/schema`（无 `diff`）/`/validate` | 总是套用编辑 | 渲染/校验路径；`/schema?diff=` 比较的是磁盘上的两份文件 |
| `/groupby` `/pivot` `/chart` `/analyze` `/stats` | 可选，`apply-edits=1` | 分析前先物化 |
| `/filter` `/search` | 可选 | 响应里的 `source` 说明扫的是 `file` 还是 `edited` |
| `/export` `/sort` `/copy` | 可选 | 输出是编辑后的表（sort/copy 会把编辑摊平进新文件）|
| `/merge` | 不适用 | 反解 `split` 归档；校验片号连续、`chunks=` 片数、归档内 `_manifest.txt` 的行数、忽略 macOS `._` 资源叉条目 |
| `/lookup` | 可选 | 主文件套编辑，参考文件保持原样 |
| `/sort`（写文件）/ `/export` 之外的写命令：`/dedupe` `/split` `/partition` `/columns` `/concat` `/merge` | **不支持** | 语义是「拷贝/切分源文件」，套编辑会改变操作对象的直觉 |

第 62 行附近还有 CLI 侧的说明：`--apply-edits` 与 HTTP 的 `apply-edits=1` 是同一能力。

## 有意的不一致 / Deliberate inconsistencies

这些地方**看起来不一致**，但每一条都是权衡后的选择。集中写在这里，免得分散在各节里被当成 bug：

| 现象 | 为什么这样 | 如果在意，怎么办 |
|---|---|---|
| `/filter` `/search` 默认看不到编辑 | 逐字节扫描；套用编辑要先物化整张编辑后的表 | 加 `apply-edits=1`，或用 `/rows?where=` |
| `sort` 不交给 DuckDB | 原生 `smart_cmp` 是「两边都能解析成数字才按数字」，SQL 的 `ORDER BY` 复现不了 | 传 `--engine` 会被**明确拒绝**并说明原因 |
| `dedupe` 不交给 DuckDB | 「保留首次出现」需要稳定行序，SQL 不承诺扫描顺序 | 传 `--engine` 会被**明确拒绝**并说明原因 |
| `concat --by-name` 只在列集合相同时按名对齐，集合不同仍报错（不补空列）|
| `concat --engine auto` 永远原生 | 拷贝字节比读值快 3 倍（18 MB×2：0.07 s vs 0.20 s）| 显式 `--engine sql` |
| `chart` 两引擎顺序可能不同 | 原生保留首次出现顺序，SQL 按标签排序 | 数值一致；顺序差异已在文档写明 |
| `--engine auto` 阈值 8 MB | 4–9 MB 是实测交叉点，三个命令一致 | 显式指定引擎 |
| `/spec` 参数表是严格模式的依据 | 未声明的参数会 400（默认开启）| 客户端自加参数时设 `CSVX_STRICT_PARAMS=0` |

## 哪些端点看得到编辑 / Which endpoints see the journal

渲染/编辑路径的核心承诺是「读到的是编辑后的表」。逐个核对：

| 端点 | 看得到 journal 编辑 |
|---|---|
| `/rows` | ✅（含 `where`/`order` 的 SQL 路径）|
| `/cell` | ✅ |
| `/style`（条件格式）| ✅（按编辑后的值判定规则）|
| `/columns`（列清单）| ✅（改名/删列后清单跟着变）|
| `/meta` | **第三种：报文件的物理事实**（行数/字节数/表头/编码）+ `edited` 计数；编辑后的表头看 `/columns`、行看 `/rows`。不是「看不到」，是「不假装是同一个东西」|
| `/journal` `/sheet` | ✅（且三处操作数一致）|
| `/validate` | ✅（规则按编辑后的值判定）|
| `/export` | **默认只读基文件**（`where` 交给 DuckDB，看不到 journal）；`apply-edits=1` 先物化编辑后的表再导出 |
| `/filter` | **默认只扫基文件**；`apply-edits=1` 扫编辑后的表，行号与 `/rows` 对齐 |
| `/search` | **默认只扫基文件**；`apply-edits=1` 扫编辑后的表 |

`/export` 一行与「默认只扫基文件」的 `/filter` `/search` 是**已知且有意的差异**：它们走 SQL 或逐字节扫描，
那正是渲染路径不承诺做的事。代价是**编辑后行号可能与基文件行号不同**（插入/删除时）。

选路建议：要看编辑后的数据用 `/rows?where=`；要的是「基文件里哪些行命中」用 `/filter`。
这个差异现在有测试钉住（`filter_and_search_read_the_base_file_not_the_journal`），
以后谁改掉它都必须显式改测试。

## 不变量与它们的守卫 / Invariants and their guards

注释里的「绝不」「永远」是最容易在重构中失效的承诺。逐个核对过的：

| 承诺 | 测试 |
|---|---|
| 基文件永不改写，编辑只在 journal | `workbook::tests::window_applies_edits_without_rewriting_the_file` |
| 排序绝不拆开带引号换行的记录 | `sort::tests::keeps_multiline_records_intact` |
| 空单元格不算「同一个值」重复 | `validate::tests::blank_cells_are_not_duplicates` |
| 被截断的 journal 记录不得以半截值生效 | `journal::damaged_tests::any_cut_point_loses_only_the_unfinished_record` |
| 崩溃落在 journal 与 redo 之间不得重复一条撤销 | `journal::damaged_tests::a_crash_between_the_two_files_does_not_double_the_undo` |
| 多个写入者不得丢编辑 | `journal::damaged_tests::concurrent_writers_do_not_lose_an_edit` |

| 单格读取必须看到 journal 编辑 | `serve::a_cell_read_sees_the_edit` |
| 条件格式必须按编辑后的值判定 | `serve::style_judges_the_edited_value` |
| 哪些端点看得到编辑（矩阵）| `serve::the_journal_visibility_matrix_holds` |
| 引擎等价不只看结果，也要看错误 | `engine::chart_engines_agree_on_how_a_missing_column_fails`、`engine::groupby_and_pivot_agree_on_how_a_missing_column_fails`（全部五个引擎端点已测：chart、groupby、pivot、lookup（含主键与参考键）、concat（表头不一致））|
| 校验规则按编辑后的值判定 | `serve::validation_judges_the_edited_value` |
| 文档里写的响应字段必须真的存在 | `serve::documented_fields_are_fields_the_endpoint_returns`（覆盖 /health /meta /metrics；其他端点文档行里反引号多为查询参数，无法机械区分）|
| split 再 merge 必须逐字节还原输入 | `splitter::tests::split_then_merge_reproduces_the_input`（1200 行，含引号换行与非 ASCII，2 线程）|
| 并行读者绝不共享或拆开一条记录 | `splitter::tests::threads_do_not_break_records_with_quoted_newlines`（1200 行，每三行一处带引号换行，4 线程与单线程逐字节比对）|

这条测试有讲究：行数必须超过 `CHECKPOINT_INTERVAL`（1000），否则所有读者都从字节 0 开始，
测到的只是「各自从头读」，而不是「从不同 checkpoint 起跳」时会不会切坏边界。

## 内存预算真的不随分片数放大吗 / Does 内存预算参数 stay a budget (measured)

89 MB 文件，`split`，同一台机器，读 `/usr/bin/time -l` 的 `maximum resident set size`：

| 配置 | 峰值 RSS |
|---|---|
| 16 片、64 MB 预算、4 线程 | **126 MB** |
| 16 片、64 MB 预算、1 线程 | **133 MB** |
| 4 片、64 MB 预算、4 线程 | **138 MB** |
| 16 片、8 MB 预算、4 线程 | **78 MB** |

结论与读法：

- **没有放大**：若每片各拿 64 MB，16 片就是约 1 GB；实测始终在 126–138 MB。
- 8 MB 与 64 MB 相差约 48 MB，接近「预算增加 56 MB」，说明预算确实在一次总量上生效。
- **但 126 MB 不等于 64 MB**：进程本身有约 70 MB 常驻开销（二进制、解码、缓冲）。文档里的
  「总预算」指 I/O 缓冲那部分，不是整个进程的 RSS——这两件事不要混。

## 下载大文件的内存 / Downloading a big file costs almost nothing (measured)

89 MB 文件，服务端限速下载（6 MB/s，持续约 15 秒），期间采样进程 RSS：

| 时刻 | RSS |
|---|---|
| 启动后、下载前 | **8.2 MB** |
| 下载进行中峰值 | **10.2 MB** |

收到的文件与服务端文件 **sha256 相同**。

这条数字的意义：`/download` 以前用 `std::fs::read` 整份读入，89 MB 文件等于 89 MB 常驻；
现在按 1 MB 分块，**多出约 2 MB**。所以「几 GB 导出不会打爆内存」不再是推测。

## 分片的多线程开关 / Is parallel splitting worth it (measured)

89 MB 的文件、切成 4 份、默认 zstd：

| 线程 | 墙钟 |
|---|---|
| 单线程（-t 1） | **447 ms** |
| 四线程（-t 4） | **182 ms** |

**2.5 倍**，而且两次产出的归档**逐字节相同**（sha256 相同）——并行只改变了速度，没有改变结果。

适用范围说明：这是单机、热缓存、SSD 上的一次测量。机械盘或冷缓存上带宽会成为瓶颈，
加速比例会下降，但不会变慢（并行读者只是从不同 checkpoint 起跳）。小文件（<几 MB）
没有收益，脚本里不必加 多线程参数。

## 首屏与跳转 / First screen and jumps (measured)

`.idx` 稀疏索引（每 1000 行一个锚点，2.5 GB / 78 万列的文件上只有 5.4 KB）让窗口读取**不需要从头扫描**。实测（2.5 GB、684 917 行、782 列）：

| 请求 | 耗时 |
|---|---|
| `/rows?start=0&count=50` | **0.014 s** |
| `/rows?start=50 000&count=50` | **0.011 s** |
| `/rows?start=600 000&count=50`（重复三次）| **0.021 s → 0.0095 s → 0.012 s** |
| `/rows?start=123 456&count=50`（另一个窗口）| **0.010 s** |

四点说明：
- 第一次请求若索引尚未建立，代价在建索引阶段，之后稳定在十几毫秒；
- 跳到最后一行与跳到第一行**几乎一样快**，因为两者都只从最近的锚点开始解析；
- **缓存带来的收益很小**（21 ms → 10 ms 量级），真正的功劳在稀疏索引；不要为了「让缓存更热」重写读取路径，
  它已经在能被感知的范围内了。
- 因此「首屏慢」不是当前瓶颈：**不要为了它重写读取路径**。真要优化，先测一遍再动手。

## 引擎选择 / Which engine runs

同一份分析有两种实现：仓库内的流式实现（native）和 DuckDB（sql）。`--engine auto` 按输入大小选，
判据来自实测而不是偏好：

| 命令 | 小文件（20 120 行） | 2.5 GB / 782 列 | auto 的选择 |
|---|---|---|---|
| `groupby` | native 0.026 s | DuckDB 2.3 s（native 13.1 s）| ≥8 MB 用 sql |
| `chart` | native | DuckDB 2.2 s | ≥8 MB 用 sql |
| `pivot`（写文件）| native | DuckDB | ≥8 MB 用 sql |
| `view --order --count N` | native 0.05 s | DuckDB 2.6 s（native 29.8 s）| ≥8 MB 且无编辑时用 sql |
| `view --where … --order …` | — | 同左 | ≥8 MB、无编辑、且 `--where` 能精确翻译成 SQL 时用 sql；关系比较（`>` `>=` `<` `<=`）不能精确翻译，退回 native 并打印 note |

### journal 半损坏时的行为

journal 是逐条追加的 CSV 记录。进程在写入中途崩溃时，最后一条记录可能不完整：

- **完整记录照常生效**——不会因为末尾坏了就丢掉全部编辑；
- **坏记录被跳过并计数**：CLI 在 stderr 警告，HTTP `/rows` 的 `damaged` 字段、
  `/journal` 的 `damaged.operations` 都会带上这个数；
- 侧车 `<file>.journal.redo` 同样处理。

**尾记录必须带换行才算完整**：一条被截断的记录可能仍能解析（`set,1,city,CDM` 是合法的
set-cell），如果照常应用，就会把一个**被截短的值**写进表格而不报错。所以读取时要求记录以
换行结尾，否则计为损坏——宁可丢一条编辑，也不悄悄写入半个值。

**两个文件的写入顺序**：journal 与 `.journal.redo` 无法一起原子替换，所以 `save` 定下顺序——
redo 为空时**先删 redo 再写 journal**（崩在中间不会留下「该被新编辑丢弃的 redo」）；redo 非空时
**先写 redo 再写 journal**（崩在中间会让同一条操作同时出现在两个文件里）。读取时做一次对账：
redo 末尾若与 journal 末尾相同，就把它从 redo 里去掉，避免下一次 redo 把同一条操作应用两次。

**不会自动修复文件**：跳过是读取时的行为，改写 journal 是另一件事（会改变用户的编辑历史），
所以工具只报告不擅自动手。

### 行号为什么必须原生

`/filter` 与 `search` 返回**文件里的行号**（1-based），`/rows?start=` 直接按这个位置取行。
要交给 DuckDB，就需要 `row_number()` 与文件物理顺序一致——而 DuckDB **不承诺**跨计划、
跨版本、跨线程数的扫描顺序。

实验记录（2026-11-20）：**200 000 行**与 **2 000 000 行**文件上，从头 `LIMIT 5` 与
`OFFSET 尾端` 查询都仍按文件顺序返回（首行 1，末行等于总行数）。20 000 行上的 canary 见下。
（`crates/core/src/sql.rs` 里的 canary 测试 `a_duckdb_scan_returns_rows_in_file_order_here`）。
**「目前如此」不等于保证**，所以行号仍由原生扫描产生；这个测试的作用是：一旦哪天不再成立，
我们能立刻看到，而不是等到用户发现点错了行。

**没有迁移的：**

- `sort`（全表写新文件）：两个理由。**语义**上，原生比较是「两边都能解析成数字才按数字，
  否则按字符串」（`smart_cmp`），没有任何一条 SQL `ORDER BY` 能对混合列复现它，换引擎会改变
  排序结果；**成本**上，DuckDB 要解析并重写全部列，而原生对非排序列只做字节拷贝，2.5 GB /
  782 列实测 34 s。
- `dedupe`：原生语义是「保留首次出现」，SQL 的 `GROUP BY` / `row_number()` 没有稳定的行序，
  换过去会悄悄改变保留哪一行。
- 渲染/编辑路径（`/rows`、公式列、条件格式、写保护）：必须看到 `base + journal`，交给 DB
  就得先物化整份文件。
- `/filter` `/search` 的 **SQL 分页**：除了行序不承诺，还有一个契约问题——这两个端点的响应要
  报 `scanned`（扫了多少行），SQL 路径要么再扫一遍 `count(*)`、要么把 `scanned` 变成 null。
  在此之前，行号仍由原生扫描给出；`apply-edits=1` 时扫描编辑后的表，行号与 `/rows` 对齐。

**资源上限**：所有 DuckDB 连接经 `open_limited()`，默认 512 MB 内存、线程上限 8、溢出落盘；
可用 `CSVX_SQL_MEMORY_MB` / `_THREADS` / `_TEMP_DIR` 覆盖。

## 解压安全 / Archive safety

`extract` 解压前会**逐个校验成员名**：绝对路径、`..`、反斜杠、盘符前缀全部拒绝，
并且**在写任何文件之前**校验完，所以恶意压缩包不会留下半截输出。

这一点对 tar 由 `tar` crate 自己做（它跳过越界成员），对 zip 是本项目自己做的
——之前没有做，`../escaped.txt` 真的写到了输出目录外面。

## 帮助 / Help

`csvx --help` 是**任务视图**（「你想做什么 → 用哪条命令」）；
`csvx <command> --help` 是**选项视图**（这条命令能传什么）。

```
$ csvx pivot --help
csvx 0.96.0 — pivot

  --cols               Pivot: column key
  --max-cells          Pivot: matrix cells allowed
  --qualify            Pivot: prefix each measure with its column key
  --values             Pivot measures, e.g. "amount:sum"
  ...
```

在此之前，60 个选项**只存在于代码里**——`--get`、`--max-cells`、`--qualify`、`--check`
这些能改变结果的参数，`--help` 一个字都没提。有一个测试读 `main.rs`、找出每条命令
真正解析的参数，再跑 `<command> --help` 比对：**新增参数忘了写帮助会直接红。**
