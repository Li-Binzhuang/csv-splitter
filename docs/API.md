# HTTP API

`csv-splitter serve --root DIR [--addr 127.0.0.1:8787]`

服务绑定回环地址，只读 `--root` 下的文件（路径经过 `canonicalize`，越界直接拒绝），
开启 CORS。除 `POST` 之外全是 `GET`；`GET /` 返回内置浏览页面。

## 约定 / Conventions

**错误统一形状**（4xx）：所有读端点在没有必需参数、或参数值非法时都返回这个形状
（有测试逐个端点验证：`bad_requests_always_carry_a_reason_and_a_hint`）。

```json
{"error":"by is required","parameter":"by","hint":"pass by=<column name>"}
```

`parameter` 指向出问题的参数，`hint` 说明该传什么。用法错误是 400，数据/引擎问题
是 422，未知路径是 404（响应里带 `endpoints` 与 `writes` 两个清单）。

**数字参数不会静默回退**：`start=abc` 是 400，不是「按 0 处理」。
静默回退会让调用方用错窗口翻页，然后把问题算到数据头上。

**窗口统一形状**（`/rows`）：

```json
{"start":0,"total":1000,"columns":["id","city"],
 "frozenColumns":["id"],"rows":[["1","CDMX"],["2","Puebla"]]}
```

`total` 是逻辑行数（编辑后）；`frozenColumns` 是需要固定的前缀列，行里仍然是全部列值。

**写入统一形状**：同步操作用 `POST /edit`（返回 `{"op":…,"operations":n}`），
长任务用 `POST /split`、`/merge`、`/sort`、`/dedupe`、`/partition`、`/lookup`、`/concat`、`/columns`、`/warm`
（返回 `{"job":"job123"}`，再用 `GET /job?id=job123` 轮询：`state` 走到 `done` 即完成，
期间 `fraction` 是进度）。

## 只读 / Reads

| 端点 | 参数 | 说明 |
|---|---|---|
| `GET /health` | — | 存活检查 + 根目录 + `duckdb`（本构建是否编入 DuckDB）|
| `GET /meta` | `path` | **基文件**的行数 `rows`、字节数 `bytes`、表头 `headers`、`delimiter`、`encoding`；另有 `edited`（journal 里的操作数）：有编辑时行/列数仍按文件计，编辑后的表头看 `/columns`、行看 `/rows` |
| `GET /rows` | `path` `start` `count` `where` `order` `desc` `sql` `format`（`format=csv` 把该窗口按编辑后的值输出为 CSV，是拿到编辑后文本的唯一途径）| 分页窗口；`where`/`order` 走 SQL，**套用编辑与公式列**。三条路径（普通/筛选/排序）返回同一组字段：`start` `count` `requested` `limit` `total` `columns` `frozenColumns` `rows` `edited` `operations`（筛选/排序额外有 `hasMore`） |
| `GET /sheet` | `path` | 列、行数、编辑数、能力清单、列布局、冻结列、是否已建索引（`indexed` / `indexBytes`） |
| `GET /book` | — | 工作簿：根目录下所有 CSV 及其状态 |
| `GET /stats` | `path` `sample` | 采样统计 |
| `GET /analyze` | `path` `distinct` | 全列画像 `distinctCap` |
| `GET /chart` | `path` `x` `y` `series` `type` `maxPoints` `engine` | 图表数据；`engine=auto\|native\|sql` |
| `GET /groupby` — 响应含 `engine`（实际执行的引擎） | `path` `by` `agg` `order` `desc` `maxGroups` `engine` | 分组聚合；`engine=auto\|native\|sql`，未编译 sql feature 时 `sql` 返回 400 |
| `GET /pivot` | `path` `rows` `cols` `values` `qualify` `maxColumns` `maxCells` `engine` | 透视矩阵 |
| `GET /chart` | `path` `x` `y` `series` `type` `sortByValue` `desc` `maxPoints` | 图表数据 |
| `GET /style` | `path` `rules` `start` `count` | 条件格式：该上色的单元格 |
| `GET /layout` | `path` `column` `key` `value` | 列宽/显示名/冻结/数字格式（带 `column` 即写入） |
| `GET /edit` | `path` | 待生效的编辑操作 + 逻辑行数；`action` `row` `col` `value` `expect` 属 POST，GET 收到返回 400 并指路 |
| `GET /filter` | `path` `where` `limit` | 命中行的行号，**1-based、表头不计**；配合 `/rows` 时用 `start = 行号 - 1`；超过查询预算返回 504（同 `/query` 的提示） |
| `GET /search` | `path` `q` `columns` `case` `exact` `regex` `limit` | 跨列搜索；`regex=1` 用条件格式那套模式子集；超过查询预算返回 504 |
| `GET /schema` | `path` `diff` `sample` `delim` `name` | 列清单；带 `diff=<另一个文件>` 时返回列集合对比（`same` / `removed` / `added` / `moved`），与 `schema --diff --json` 同一份文档 |
| `POST /edit` | `action=fill&cols=a,b&from-row=&to-row=&value=` | 多列区间填充，**一次操作一次撤销**（`col=` 仍是单列形式）|
| `POST /edit` | `action=insert-after-key&key=&value=` / `insert-before-key` | 按 **rowKey** 插行（`b:12`/`v:3`，来自上一次 `/rows`）；key 不存在 404，格式错 400 |
| `POST /edit` | `action=set-formula&col=&value=` | 给**已有列**重套公式（一次撤销）；自引用或未知列在写入前就 400 |
| `GET /validate` | `path` `checks` `delim` `limit` | 数据校验，与 CLI `validate --json` **同一份报告**：`ok` / `violations` / 每规则计数；**只读不写**；违规数据仍是 **200**（请求没错，数据错了）。`limit=N` 只校验前 N 行，报告里 `checked` 与 `partial` 明说这是前缀；2.5 GB 全量实测 **34.9 s**，`limit=200` 为 **0.11 s**；不传 `limit` 且超过查询预算时返回 504，提示用 `limit=` |
| `GET /files` | `ext` `limit` | 根目录下可读文件清单 |
| `GET /download` | `path` | 原样下载文件；按 1 MB 分块流出，不整份读入内存；支持单段 `Range`（206 + Content-Range，越界 416），可用 curl -C - 断点续传 |
| `GET /export` | `path` `sql` `where` | 导出（查询结果或筛选结果）为 CSV |
| `GET /query` | `path` `sql` `limit` | 一次性 SQL 结果（不分页） |
| `GET /job` | `id` | 任务状态：`state` `phase` `fraction` `output` `error` |
| `GET /jobs` | — | 最近的任务列表 |

## 写入 / Writes

| 端点 | 参数 | 说明 |
|---|---|---|
| `POST /edit` | `path` `action` … | action 来自 `serve.rs` 的 `EDIT_ACTIONS`：`set`、`insert`、`delete`、`replace`、`copy`、`formula`、`drop`、`rename`、`undo`、`redo`、`export`。参数错误时响应的 `hint` 就是这份清单 |
| `POST /split` | `path` `splits` `out` `format` `threads` | 切分（`splits` 为片数） |
| `POST /merge` | `out` `format` | 合并（根目录下的分片） |
| `POST /sort` | `path` `by` `desc` `out` | 外部排序 |
| `POST /dedupe` | `path` `by` `out` | 去重 |
| `POST /partition` | `path` `by` `out` `format` `maxPartitions` | 按列值拆成多个文件 |
| `POST /lookup` | `path` `key` `from` `get` `out` `refKey` `maxKeys` | 跨表查找（job）：按 `key` 从 `from` 拷 `get` 列；未命中写 `#N/A`，`engine=auto\|native\|sql`，`key`/`refKey` 可写复合键 |
| `POST /convert` | `path` `out` `from` `to` | 编码转换（job）：默认自动探测源编码，`to=utf-8` 或 `utf-8-bom`；未知编码 400 |
| `POST /columns` | `path` `out` `keep` `drop` `rename` `delim` | **写**的列变换（job）：产出一个列集合不同的新文件；源文件不动。与 `GET /columns`（只读的列清单）是两个方向 |
| `POST /concat` | `paths` `out` `ignoreHeader` `delim` | 拼接多个文件（job）；每个输入与输出都过 root 守卫，`engine=auto\|native\|sql` |
| `POST /warm` | `path` `sql` `start` `count` | 预热查询缓存 |
| `POST /index` | `path` `interval` | 后台建稀疏索引（翻页前先跑，避免首个请求里含着全表扫描） |

## 编辑动作 / Edit actions

| `action` | 参数 | 说明 |
|---|---|---|
| `set` | `row=N` **或** `where=列=值`，`col` `value` | 单格或按条件批量（批量存成一条规则） |
| `insert` | `row` `value`（CSV 行） | 插入行 |
| `insert-after-key` / `insert-before-key` | `key` `value` | 按 **rowKey** 插行（`b:12`/`v:3`）；key 不存在 404，格式错 400 |
| `delete` | `row` `count` | 删除行 |
| `replace` | `from` `to` `col` | 查找替换 |
| `drop` / `rename` | `name` / `from`,`to` | 删列 / 改名 |
| `insert-column` | `name` `value` **或** `formula`，`after` | 插入常量列或公式列 |
| `formula` | `name` `value` | 追加公式列 |
| `set-formula` | `col` `value` | **重算已有列**（一次撤销）；自引用/未知列写入前 400 |
| `copy` | `from=A1:B2` `to=B3` | 区域复制（A1 风格范围） |
| `split-column` | `col` `by` `into` | 一列拆成多列（最后一片吃下剩余部分） |
| `fill` | `col` **或** `cols` `from-row` `to-row` `value` | 区间填充（1-based，闭区间）；`cols=a,b` 是**一次操作**（一次 undo）；精确 `set` 优先于填充 |
| `delete-where` | `where` | 删除所有命中行，**一条操作**（一次 undo 全恢复）；命中点超过 1000 段会拒绝并建议改用 `/filter` |
| `undo` / `redo` | — | 撤销最后一条 / 重做 |
| `export` | `out` | 物化到文件（返回 job） |


> `/spec（`errors` 覆盖全部 17 个状态码）` 的 `params` 现已覆盖**全部端点**（参数表按 handler 实际读取的 key 补齐），
> 并由 `crates/cli/tests/contract.rs` 双向断言：`/spec` 不能声明不存在的端点，已服务的
> 端点也不能缺席。参数「是否真的被 handler 读取」这一层仍然无法用机械检查证明——
> 参数常经辅助函数传入（`resolve_out(root, params, "out")`）——所以那一层靠测试逐个
> 端点断言行为，而不是靠字符串匹配。
>
> 其中「声明了但读侧拒绝」的参数单列在 `writeOnlyParams`：`/edit` 与 `/columns` 的写参数
> 由 GET 收到时返回 400 并指路，这张表与处理器共用同一个常量，发布出去不会与行为漂移。

## 常用示例

```bash
# 打开工作簿
curl "http://127.0.0.1:8787/book"

# 翻页（编辑后的表）
curl "http://127.0.0.1:8787/rows?path=orders.csv&start=0&count=50"

# 按条件筛选（会看到编辑）
curl "http://127.0.0.1:8787/rows?path=orders.csv&where=city%3DCDMX"

# 批量改
curl -X POST "http://127.0.0.1:8787/edit?path=orders.csv&action=set&where=city%3DCDMX&col=amount&value=0"

# 透视
curl "http://127.0.0.1:8787/pivot?path=orders.csv&rows=city&cols=month&values=amount:sum"

# 条件格式
curl "http://127.0.0.1:8787/style?path=orders.csv&rules=amount%3E1000%3Abg%3Dred"

# 图表数据
curl "http://127.0.0.1:8787/chart?path=orders.csv&x=month&y=amount:sum&series=city"

# 物化编辑结果（长任务）
curl -X POST "http://127.0.0.1:8787/edit?path=orders.csv&action=export&out=fixed.csv"
curl "http://127.0.0.1:8787/job?id=job123"
```


## 索引 / Index

翻页在没有索引的文件上要先扫一遍（60 MB 约 0.5 s，10 GB 就是几十秒）。
`GET /sheet` 的 `indexed` / `indexBytes` 告诉你索引在不在；
`POST /index?path=` 可以在翻页前先建好，它返回 job id，不占着请求：

```
POST /index?path=big.csv   →  202 {"job":"job123"}
GET  /job?id=job123        →  state: done
```

前端可以这样用：打开文件先看 `indexed`，false 就调一次 `/index`，等 job 完成再翻页。

## `capabilities` 是承诺 / Capabilities are a promise

`GET /sheet` 的 `capabilities` 决定前端画哪些按钮，所以它必须与实际能执行的操作
**双向一致**：

| 方向 | 违反的后果 |
|---|---|
| 做了但没列 | 用户看不到这个功能 |
| 列了但没做 | 用户点了报错 |

当前列表 18 项：`window`、`edit.set`、`edit.set-where`、`edit.insert`、`edit.delete`、
`edit.replace`、`edit.replace-regex`、`edit.paste`、`edit.copy`、`edit.undo`、
`edit.redo`、`column.add`、`column.insert`、`column.rename`、`column.drop`、
`column.split`、`column.formula`、`export`。

## `?path=` 的四种错误 / Four path mistakes

不解析时有四种不同的说法，因为它们是四种不同的错误：

| 情况 | 消息 |
|---|---|
| 没给 `path` | `path is required` |
| 文件不存在 | `no such file inside the served directory` |
| 指向目录 | `path is a directory, not a file` |
| 逃出 root | `path is outside the served directory` |

（早先这四种都报「path missing or outside root」，把「文件名打错」说成「沙箱问题」。）
## 性能 / Measured

同一个 215 MB / 500 万行文件，release 构建，本机：

| 场景 | 结果 |
|---|---|
| `/sheet`（含建索引）| 66 ms |
| `/rows` 任意偏移 | **66 ms 首屏（含建索引），之后 6–7 ms** |
| `/rows?where=…` 首屏（物化结果）| 296 ms |
| 同一筛选条件后续页 | 15 ms |

`count` 上限 2000，超了会报 `requested` / `count` / `limit` 三个字段。


## `GET /columns`

只读：列出列名/类型。`keep` `drop` `rename` `out` 是 **POST /columns** 的参数，GET 收到会返回 400
并指路（`/spec` 按路由列参数、不分方法，所以它们出现在同一条声明里）。

The framework's first question: which columns does this sheet have?

```bash
curl "http://127.0.0.1:7811/columns?path=big.csv&sample=10"
```

```json
{"path":"big.csv","rows":684917,"count":782,
 "columns":[{"name":"base_time","index":1,"kind":"text","empty":0,"sampled":10},
            {"name":"apply_id","index":2,"kind":"number","empty":0,"sampled":10},
            ...]}
```

- **Never truncated**: 782 columns come back as 782 entries.
- `index` is 1-based, like every row and column number on the wire.
- On a 2.5 GB / 782-column file: `?name=` answers in **3 ms**, the full list in **0.66 s**.
- `kind` is one of `number` / `date` / `text` / `empty` / `mixed`, sampled from the
  first `sample` rows (default 10) — enough to choose an editor, without a full scan.
- `empty` counts the sampled blank cells.

### Filtering the list

```bash
curl "…/columns?path=t.csv&kind=number&sample=50"
# {"count":2,"totalColumns":3,"shown":2,"kindFilter":"number","columns":[…]}

curl "…/columns?path=t.csv&limit=20"
# first 20 columns, with totalColumns and shown so nothing is silently lost
```

`kind=` takes one of `number`/`date`/`text`/`empty`/`mixed` (anything else is a 400
listing the options). `limit=` truncates for rendering but always reports
`totalColumns` and `shown`.

### Existence check

```bash
curl "http://127.0.0.1:7811/columns?path=big.csv&name=cust_no"
{"name":"cust_no","exists":true,"index":3}

curl "http://127.0.0.1:7811/columns?path=big.csv&name=nosuch"
{"name":"nosuch","exists":false}
```

One question, one answer: a client does not download 782 columns to learn that one is
missing. On a 2.5 GB / 782-column file this answers in **9 ms**.

## `POST /edit?action=set&expect=…` (compare-and-set)

A front-end edits a window it fetched a moment ago. Without a guard, a blind write can
replace a value nobody looked at. `expect=` makes the write conditional:

```bash
curl -X POST "http://127.0.0.1:7811/edit?path=t.csv&action=set&row=1&col=city&value=Z&expect=CDMX"
# 200 -> written

curl -X POST "http://127.0.0.1:7811/edit?path=t.csv&action=set&row=1&col=city&value=Z2&expect=CDMX"
# 409 -> {"error":"the value changed; nothing was written","parameter":"expect",
#         "hint":"re-read the row and retry without expect=","actual":"Z","expected":"CDMX"}
```

`409` is distinct from `400`: the request is well-formed, the world moved. Nothing is
written on refusal, and `actual` says what the value is now.



## 状态码契约 / Status-code contract

一个错误类别对应一个码，客户端可以按码分支：

| 码 | 什么时候 | 例子 |
|---|---|---|
| **400** | 请求本身有问题（参数、路径、列名、SQL 语法）| `path=nope.csv`、`order=nosuch`、`sql=SELECT nope`、`path=../etc/passwd` |
| **404** | 请求合法但目标不存在 | 未知端点；`/cell?row=999` 超过末尾 |
| **405** | 方法不对 | `PUT /rows`、对 `POST /split` 发 GET |
| **409** | 目标在你之前被改了 | `expect=` 不匹配（带 `actual`/`expected`）|
| **422** | 文件读不了（编码/格式）| UTF-16 文件、损坏的 CSV |
| **504** | 引擎被停止（超时）| 查询超过 30 秒；响应说明「引擎已停止、连接已关闭，没有任务仍在跑，缩小范围重试是安全的」 |
| **414** | 请求参数太长 | 查询串超过 16 KB |

**有测试逐条钉住这张表**（`failure_kinds_have_one_status_code_each`），所以「路径错误返回 400 还是 404」不是靠记忆。

## 端点总表 / Endpoint index

### 路径错误的四种消息

| 消息 | 意思 |
|---|---|
| `path is required` | 没传 `path` |
| `no such file inside the served directory` | 文件不存在 |
| `path is a directory, not a file` | 传了目录 |
| `path is outside the served directory` | 越界 |

**四种错分开说**，因为客户端处理方式不同（补参数 / 提示用户 / 换接口 / 拒绝）。

| 方法 | 路径 | 作用 |
|---|---|---|
| GET | `/health` | 存活 |
| GET | `/meta` | 行数、字节数、表头、分隔符、编码 |
| GET | `/rows` | 窗口（`start`/`count`/`where`/`order`/`columns`/`start-col`/`keys`）|
| GET | `/columns` `/schema` | 列清单（`name=` 查存在性，`kind=`/`limit=` 过滤）|
| GET | `/cell` | 单格 + 稳定行 key |
| GET | `/journal` | 操作栈 + redo 栈 + 损坏计数 |
| GET | `/sheet` | 概况：操作数、最后一条、capabilities、layout、indexed |
| GET | `/capabilities` | 能力发现（reads/writes/actions/limits/engines/duckdb/strictParams）|
| GET | `/spec` | **机器可读契约**：端点、参数、响应字段、错误码；代码生成，有测试对齐真实响应 |
| GET | `/metrics` | 请求数、p50/p95、慢请求、状态码分布、按路由的计数、job 数、`engines`（各引擎执行次数）、`journalDamaged`（累计读不出的 journal 记录数）、`lastDamagedFile`（最近一次出问题的文件）|
| GET | `/health` | 存活 + 就绪（`ready` = 根目录可读），带版本 |
| GET | `/edit` | **只读**：journal 状态 |
| POST | `/edit` | 写入（`action=` 见下表，`expect=` 可用）|
| GET | `/export` | 把查询结果当 CSV 直接下载（不写文件）|
| POST | `/edit?action=export` | 物化到文件（异步 job）|
| GET | `/query` | SQL（只读，`limit` 上限 1 万并回显，30 秒超时后中断）|
| GET | `/stats` `/analyze` `/filter` `/search` `/validate` | 画像、检索与校验 |
| GET | `/groupby` `/pivot` `/style` `/chart` `/layout` | 聚合与展示元数据 |
| GET | `/book` `/files` `/download` | 目录、书、下载（`book?root=` 不存在时 400；`max`/`limit` 截断时带 `total`/`hidden`）|
| GET | `/job` `/jobs` | 异步任务状态 |
| POST | `/split` `/merge` `/sort` `/dedupe` `/partition` `/lookup` `/concat` `/columns` `/convert` `/warm` `/index` `/parquet` | 拆分、查找、预热与后台任务（`/parquet` 建列式缓存）|

## `GET /capabilities`

One call that tells a front-end what this backend can do — generated from the same
constants the handlers use, so it cannot drift from the routes.

```json
{"version":"3.54.3",
 "duckdb":true,
 "strictParams":true,
 "engines":["native","sql"],
 "engineParam":["/groupby","/chart","/pivot","/lookup","/concat"],
 "reads":["/","/health","/meta","/rows","/columns","/sheet","/capabilities","…"],
 "writes":["POST /split","POST /merge","POST /sort","POST /dedupe","POST /partition","POST /warm","POST /edit","POST /index"],
 "editActions":["set","insert","delete","replace","copy","formula","drop","rename","undo","redo","export"],
 "limits":{"rowsPerRequest":2000,"columnsProjection":true},
 "writeGuards":["expect=compare-and-set"],
 "notes":"rows are 1-based; columns= narrows a window; expect= refuses a write unless the cell still holds that value (409)"}
```

## `GET /rows?columns=…`

A window can ask for a few columns instead of all of them:

```bash
curl "/rows?path=real.csv&start=680000&count=10"
# 782 columns → 87 KB

curl "/rows?path=real.csv&start=680000&count=10&columns=cust_no,model_prob,predict_status"
# 3 columns → 529 bytes   (same rows, same numbers)
```

The order is the order asked for, the names must exist (`400` with the available list
otherwise), and `frozenColumns` is narrowed the same way.

## 并发写入 / Concurrent writers

`POST /edit` and `csv-splitter edit` take a lock on `<file>.journal` around
read-modify-write, so two clients appending at the same moment both survive:

```bash
for i in $(seq 1 10); do csv-splitter edit set t.csv --row $i --col city --value W$i & done; wait
# journal now holds 10 operations, one per writer
```

A lock left by a killed process is treated as stale after ten seconds, so a crash
cannot wedge the file. For "did this value change under me?" the answer is not the lock
but `expect=` — the lock keeps both edits, `expect` decides whether yours should happen.


## `/sheet` 与 `/journal`：两件事各说一次

| 调用 | 给什么 |
|---|---|
| `GET /sheet` | 这个表的**概况**：`operations`（待应用操作数）、`lastOperation`（最后一条）、capabilities、layout、indexed |
| `GET /journal` | **栈本身**：全部操作 + redo 栈 + 损坏计数 |

`/sheet` 不再重复整个操作列表——一个事实只有一个地方给，两个地方迟早会不一致。

## `GET /cell?row=&col=`

One cell, by column name and 1-based row — the read half of a constrained write:

```bash
curl "/cell?path=t.csv&row=2&col=city"
{"row":2,"column":"city","value":"X","key":"b:1","total":5}
```

`404` when the row is past the end (and says the total), `400` naming `col` when the
column does not exist. `key` is the row's stable identity (`b:<index>` base row,
`v:<id>` inserted row).

## `/schema` — an alias for `/columns`

Same payload, same parameters. A front-end that thinks in "schema" should not have to
remember a second name.

## 横向翻列 / Horizontal paging

`/rows` takes `start-col=` and `count-cols=`, so a screen-sized slice of a 782-column
sheet is one request:

```bash
curl "/rows?path=t.csv&count=2&start-col=5&count-cols=5"
# columns c4..c8, only those bytes
```

## 行身份 / `rowKeys`

Every window carries `rowKeys`, parallel to `rows`: `b:<index>` for a row of the base
file, `v:<id>` for an inserted row. A selection that survives an insert or a delete is
a selection by key, never by position.


## `GET /rows?keys=b:2,b:0` — 按 key 找行 / resolving a selection

`rowKeys` 的反函数。前端保存的是 key，插入/删除之后位置变了，用这个问「它现在在第几行」：

```bash
curl "/rows?path=t.csv&keys=b:2,b:0,b:99"
{"columns":["id","city"],
 "rowKeys":["b:2","b:0"],
 "rowNumbers":[3,1],
 "rows":[["3","C"],["1","A"]],
 "missing":["b:99"]}
```

- 返回顺序就是你给的顺序；
- 不存在的 key 进 `missing`（被删掉的行是「没有了」，不是错误）；
- 一次最多 200 个 key，写错的 key 是 400，`parameter` 为 `keys`。

### 窗口上限 / the row cap

`count=5000` 会被截到 2000，但**三个字段一起说清楚**：

```json
{"count":2000,"requested":5000,"limit":2000,…}
```

`count` 是**实际给的行数**，`requested` 是你要求的，`limit` 是这次服务端的上限。
`/capabilities` 的 `limits.rowsPerRequest` 也给同一个数字。

### 过滤页的总数 / total on a filtered page

```bash
curl "/rows?path=t.csv&where=city=CDMX&count=5"
{"start":0,"count":5,"total":5,"totalExact":false,"hasMore":true, …}
```

- `totalExact: true` → `total` 就是命中总数（这一页读到了结果末尾）；
- `totalExact: false` → `total` 是**下界**（至少这么多），别拿它当总数显示；
- 过滤页**不带 `rowKeys`**：行是引擎挑出来的，不是 base 文件的 `start+i`，
  给 key 会指向错的行。要 key 就别过滤。

无 `where` 的窗口 `rowKeys` 恒有；带 `order=` 的窗口也不带 key（排序后行不是 `start+i`），
并且和过滤页一样用 `totalExact` 说明 `total` 是精确值还是下界。

## `order=` 与窗口修饰符 / window modifiers

`/rows` 支持三组修饰符，可以叠加使用：

| 参数 | 作用 |
|---|---|
| `where=city=CDMX` | 过滤（对**编辑后的值**求值）|
| `order=amount:desc,id:asc` | 排序，`col` / `col:asc` / `col:desc`，逗号多列；`desc=1` 是旧写法，仍可用 |
| `columns=id,amount` | 只要这些列（顺序按你给的）|
| `start-col=5&count-cols=10` | 横向翻列 |

三者同时用是合法的：

```bash
curl "/rows?path=t.csv&order=amount:desc&columns=id,amount,note&start-col=1&count-cols=1&count=2"
{"columns":["amount"],"rows":[["290"],["280"]],…}
```

**排序方向写错或列名里带非法字符时返回 400**，`parameter` 为 `order`，不会把原样的
SQL 错误透给客户端：

```json
{"error":"order must name a column: \"amount;DROP\" is not a column name","parameter":"order",
 "hint":"order=amount or order=amount:desc,id:asc"}
```

## 行身份 / Row identity

| key | 含义 |
|---|---|
| `b:<n>` | 原始文件里的第 n 行（0-based，表头不算）|
| `v:<id>` | 插入块 `<id>` 的一行 |

**编辑就是按这个身份存的**，所以 key 是唯一能跨插入/删除稳定的行标识。


## `GET /journal` — 撤销栈 / the undo stack

A front-end that shows "undo this change" needs the stack, not a guess. Each operation
carries the same 1-based description the CLI prints:

```bash
curl "/journal?path=t.csv"
{"path":"t.csv",
 "operations":[{"tag":"set","description":"set row 1 'city' = 'X'"}],
 "count":1,
 "redo":[{"tag":"set","description":"set row 2 'city' = 'Y'"}],
 "redoCount":1,"canUndo":true,"canRedo":true,
 "damaged":{"operations":0,"redo":0}}
```

`damaged` is non-zero only when a journal record could not be read (the same fact the CLI
warns about) — a UI can show "the history is incomplete" instead of pretending.

## Write actions / 写入动作

`POST /edit?action=` 接受这些（与 `GET /capabilities` 的 `editActions` 同源）：

| action | 参数 |
|---|---|
| `set` | `row`, `col`, `value`, 可选 `expect`（compare-and-set，409）|
| `insert` | `row`, `value=a,b,c` |
| `delete` | `row`, `count` |
| `replace` | `col` 或 `from…`（见 CLI `edit replace --help`）|
| `copy` | `from`, `to`（A1 范围）|
| `fill` | `col`, `from-row`, `to-row`, `value` |
| `insert-column` | `name`，以及 `value` 或 `formula`，可选 `after` |
| `split-column` | `col`, `by`, `into=a,b` |
| `formula` | `name`, `value=<expr>` |
| `drop` | `name` |
| `rename` | `from`, `to` |
| `undo` / `redo` | — |
| `export` | `out`，可选 `where` |

猜错动作名时，`hint` 会把这张表列出来——有测试保证它不会漏。


## 核心端点的响应字段 / response fields

### `/rows`

| 字段 | 含义 |
|---|---|
| `start` | 起始行（0-based；**对外行号是 1-based**，见 `rowNumbers`）|
| `count` / `requested` / `limit` | 实际行数 / 请求行数 / 本次上限（2000）|
| `total` + `totalExact` | 总数；过滤页读不到末尾时 `totalExact:false`（`total` 是下界）|
| `pastEnd` | `start` 超过末尾；空文件不算（看 `total: 0`）|
| `columns` / `frozenColumns` | 表头与冻结列 |
| `rows` | 二维字符串数组 |
| `rowKeys` | `b:<index>` 基行 / `v:<id>` 插入行；**过滤页没有**（行不是 `start+i`）|
| `edited` / `operations` | 这一屏是否套用了 journal，套用了几条 |

### `/columns`

| 字段 | 含义 |
|---|---|
| `rows` | 总行数；**没有索引时是 `null`**（行数要一次全表扫描，不该由「取列名」触发）|
| `count` | 返回的列数（`kind`/`limit` 过滤之后）|
| `totalColumns` | 过滤前的列数 |
| `shown` | 实际返回的条数 |
| `kindFilter` | 应用的 `kind=`，没有则 `null` |
| `columns[].name` / `.index` | 列名 / **1-based** 位置 |
| `.kind` | `number`/`date`/`text`/`empty`/`mixed`，**采样推断** |
| `.empty` / `.sampled` | 采样内空格数 / 采样行数 |
| `.distinct` / `.distinctCapped` | 采样内不同值数（封顶 64），是否触顶 |

`?name=` 时只返回 `{"name","exists","index"?}`。

### `/cell`

| 字段 | 含义 |
|---|---|
| `row` / `column` / `value` | 1-based 行号、列名、值 |
| `key` | 该行的稳定身份 |
| `total` | 总行数 |

### `/journal`

| 字段 | 含义 |
|---|---|
| `stamp` | 变更探测器（`操作数:字节数:mtime秒`），与 `/sheet` 的相同 |
| `operations[]` / `count` | 待应用操作（`tag` + 1-based 描述）与条数 |
| `redo[]` / `redoCount` | 可重做栈 |
| `canUndo` / `canRedo` | 按钮可用性 |
| `damaged` | 读不出的记录数（正常为 0）|

## 已知限制

- `where` / `order` 走 DuckDB：对**有编辑的文件**会先物化到临时表（每份源文件最多一份，
  日志变化即替换），超过 **2 GB** 直接报错让你先 `edit export`。
- 单次结果封顶：`/query` 最多 1 万行、`/rows` 单页最多 `MAX_ROWS_PER_REQUEST` 行。
- `GET /layout` 带参数即写入，是唯一"读端点会写"的地方（为了少一个端点，代价是
  语义不纯；用 `POST` 也可以）。

## 严格参数检查（可选）

请求里出现 `/spec` 未声明的参数会返回 **400**：`parameter` 指明是哪一个，`route` 指明哪个路由，`hint` 指向 `/spec`。

**默认开启**；需要发送额外参数（缓存穿透、代理注入）的客户端可以设置 `CSV_SPLITTER_STRICT_PARAMS=0` 关闭。

这条规则的价值来自它的反作用：默认开启后测试暴露了 `count`、`from-row`、`out`、`splits`、`ext`、`root`、`desc`、`cols`、`key`、`whereAny` 等十几个「辅助函数按变量名读取、表里没写」的参数，现在都补进了 `/spec`。
