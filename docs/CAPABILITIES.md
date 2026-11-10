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
| 文件元信息 | — | `GET /meta?path=` |
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
| 引号风格 | `--quotes rfc4180\|backslash\|none`：`preview` `filter` `view` `groupby` `pivot` `chart` `dedupe` `stats` `analyze` `lookup` `search` `columns` `edit export`。`sort` / `concat` / `sql` **还不接受**（按 RFC 4180 读）。检测到反斜杠转义时会自动提示 |

## 分析

| 能力 | CLI | API | 边界 |
|---|---|---|---|
| 分组聚合 | `groupby --by K --agg c:op [--apply-edits]` | `GET /groupby` | `--max-groups` 默认 100 万 |
| 透视表 | `pivot --rows R --cols C --values c:op` | `GET /pivot` | `--max-columns` 512 / `--max-cells` 200 万 |
| 图表数据 | `chart --x C --y c:op [--series S]` | `GET /chart` | `--max-points` 5000 |
| SQL | `sql f.csv "SELECT ..."` | `GET /query`、`/rows?sql=` | DuckDB，只读单语句，结果封顶 |
| 筛选 | `filter --where "c=v"` | `GET /filter`、`/rows?where=` | 支持 `= != > >= < <= ~ in empty` |
| 搜索 | `search f.csv "text" [--regex]` | `GET /search?regex=1` | 返回行号；`--regex` 用条件格式那套模式子集 |
| 跨表查找 | `lookup --key C --from R --get cols` | — | `--max-keys` 默认 500 万（公式里的 `ref()` 默认 200 万） |

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
| 插入行 | `edit insert --row N --value "a,b,c"` | `action=insert` |
| 删除行 | `edit delete --row N --count K` | `action=delete` |
| 按条件删除 | `edit delete --where "city=Puebla"`（一次操作，一次撤销） | `action=delete-where` |
| 按内容定位后修改 | `edit set --find Puebla --col city --value "..."` | — |
| 区间填充（跟着行走） | `edit fill --col qty --from-row 5 --to-row 10 --value 0` | `action=fill` |
| 一列拆成多列 | `edit split-column f.csv --col full --by "," --into "first,last,addr"` | — |
| 分页浏览（筛选/排序都看编辑后的值） | `view f.csv [--where EXPR] [--order COL[--desc]] [--find TEXT [--regex] [--nth N] [--context N]] [--start N] [--count M] [--json]` | `GET /rows?where=` |
| 查找替换 | `edit replace --from X --to Y [--col C]` | `action=replace` |
| 删列/改名 | `edit drop --name C` / `edit rename` | `action=drop/rename` |
| 公式列 | `edit formula --name N --value "EXPR"` | — |
| 撤销 | `edit undo` | `POST /edit?action=undo` |
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
逻辑 if(a,b,c) and or not isblank isnumber iferror
文本 & "字符串" left right mid trim replace concat upper lower len
数字 round abs floor ceil int sqrt mod min max
日期 year(d) month(d) day(d)     ── 取 ISO 前缀（2024-03-15，带时间也行）
列引用 amount   [客户编号]
```

`if` 只算选中的分支（`if(qty=0, 0, amount/qty)` 不会除零）。
空单元格当 0；非数字得 `#VALUE!`；引用不存在的列 → **导出前报错**。

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
| 列集合对比 | `schema a.csv --diff b.csv`（新增/缺失/移位，退出码 1）| — |
| 列过滤/截断 | — | `/columns?kind=number&limit=20`（带 totalColumns）|
| 撤销栈查询 | `edit status` | `GET /journal`（含 redo 栈与损坏提示）|
| 按 key 定位行 | `/rows?keys=b:12,v:3`（插入/删除后仍指向同一行）| — |
| 并发写入 | 两个进程同时 `edit` | journal 加锁，双方操作都保留（实测 10 并发 → 10 条）|
| 窗口选列 | `/rows?columns=a,b`（782 列 → 3 列，87 KB → 529 字节）| — |
| 列存在性 / 列元数据 | `schema --has COL`（存在 0 / 不存在 1）· `schema --json` | `GET /columns` |
| 写入约束 | `edit set --expect VALUE`（不匹配则拒绝）| `POST /edit?action=set&expect=`（409）|
| 数据校验 | `validate --check "qty>=0" --check "city:not-empty"` | — |
| 删掉不合格的行 | `edit delete --invalid "ts:date"`（一次操作，一次撤销） | — |
| 去重 | `dedupe -o out.csv [--by C]` | `POST /dedupe` |
| 分文件 | `partition --by C -o parts.tar` | `POST /partition` |
| 拼接 | `concat a.csv b.csv ...` | — |
| 导列/改名 | `columns --keep/--drop/--rename` | — |
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
| `sort` | 218 MB | **排序缓冲**（`--buf-mb`，默认 256 MB）|
| `partition` | 287 MB | 同上：先按分区列排序 |
| `dedupe`（4M 个不同键） | 285 MB | **记得住的键**（`--max-keys` 上限 1 亿）|
| `dedupe --by city`（20 个键） | **9 MB** | 键少，所以省 |

所以「内存与行数无关」的准确版本是：**与行数无关，除非那个命令必须记住每一行**
（`dedupe` 要记住见过的键；`sort`/`partition` 要一片缓冲区）。这三种都有显式上限：
`--max-keys` / `--buf-mb`，超了会报错而不是悄悄 OOM。

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

**慢的两种，原因都清楚**：

1. **宽表逐个单元格物化**。`export` / `validate` / `analyze` 走的是「每行物化成
   `Vec<String>`」的路径（782 个 `String`/行），2.5 GB 下约 40–60 MB/s；
   纯字节流过的 `filter` 是 190 MB/s。窄表看不出差别，782 列放大 15 倍。
2. **`validate` 曾经每行每规则重建一次整行探针**（782 个空 `String` × 两个规则 × 68 万行），
   实测 72 s；改成每规则复用一个探针后 61 s——剩下的仍是第 1 条的结构成本。

**已知的改进方向**（未做）：让 `RowPipeline` 用借用的 `&str`（`Cow`）而不是每格一个
`String`，宽表的导出和校验能再快 2–3 倍。这是结构性改动，需要的时候再做。

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

## 解压安全 / Archive safety

`extract` 解压前会**逐个校验成员名**：绝对路径、`..`、反斜杠、盘符前缀全部拒绝，
并且**在写任何文件之前**校验完，所以恶意压缩包不会留下半截输出。

这一点对 tar 由 `tar` crate 自己做（它跳过越界成员），对 zip 是本项目自己做的
——之前没有做，`../escaped.txt` 真的写到了输出目录外面。

## 帮助 / Help

`csv-splitter --help` 是**任务视图**（「你想做什么 → 用哪条命令」）；
`csv-splitter <command> --help` 是**选项视图**（这条命令能传什么）。

```
$ csv-splitter pivot --help
csv-splitter 0.96.0 — pivot

  --cols               Pivot: column key
  --max-cells          Pivot: matrix cells allowed
  --qualify            Pivot: prefix each measure with its column key
  --values             Pivot measures, e.g. "amount:sum"
  ...
```

在此之前，60 个选项**只存在于代码里**——`--get`、`--max-cells`、`--qualify`、`--check`
这些能改变结果的参数，`--help` 一个字都没提。有一个测试读 `main.rs`、找出每条命令
真正解析的参数，再跑 `<command> --help` 比对：**新增参数忘了写帮助会直接红。**
