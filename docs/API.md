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
长任务用 `POST /split`、`/merge`、`/sort`、`/dedupe`、`/partition`、`/warm`
（返回 `{"job":"job123"}`，再用 `GET /job?id=job123` 轮询：`state` 走到 `done` 即完成，
期间 `fraction` 是进度）。

## 只读 / Reads

| 端点 | 参数 | 说明 |
|---|---|---|
| `GET /health` | — | 存活检查 + 根目录 |
| `GET /meta` | `path` | 编码、行数、列数、分隔符、是否 ragged |
| `GET /rows` | `path` `start` `count` `where` `order` `desc` `sql` `format` | 分页窗口；`where`/`order` 走 SQL，**会套用编辑**。响应带 `count`（实际返回行数）、`requested`（请求的）与 `limit`（上限 2000） |
| `GET /sheet` | `path` | 列、行数、编辑数、能力清单、列布局、冻结列、是否已建索引（`indexed` / `indexBytes`） |
| `GET /book` | — | 工作簿：根目录下所有 CSV 及其状态 |
| `GET /stats` | `path` `sample` | 采样统计 |
| `GET /analyze` | `path` `distinct` | 全列画像 |
| `GET /groupby` | `path` `by` `agg` `order` `desc` `maxGroups` | 分组聚合 |
| `GET /pivot` | `path` `rows` `cols` `values` `qualify` `maxColumns` `maxCells` | 透视矩阵 |
| `GET /chart` | `path` `x` `y` `series` `type` `sortByValue` `desc` `maxPoints` | 图表数据 |
| `GET /style` | `path` `rules` `start` `count` | 条件格式：该上色的单元格 |
| `GET /layout` | `path` `column` `key` `value` | 列宽/显示名/冻结/数字格式（带 `column` 即写入） |
| `GET /edit` | `path` | 待生效的编辑操作 + 逻辑行数 |
| `GET /filter` | `path` `where` | 命中行的行号 |
| `GET /search` | `path` `q` `columns` `case` `exact` `regex` `limit` | 跨列搜索；`regex=1` 用条件格式那套模式子集 |
| `GET /files` | `ext` `limit` | 根目录下可读文件清单 |
| `GET /download` | `path` | 原样下载文件 |
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
| `POST /warm` | `path` `sql` `start` `count` | 预热查询缓存 |
| `POST /index` | `path` `interval` | 后台建稀疏索引（翻页前先跑，避免首个请求里含着全表扫描） |

## 编辑动作 / Edit actions

| `action` | 参数 | 说明 |
|---|---|---|
| `set` | `row=N` **或** `where=列=值`，`col` `value` | 单格或按条件批量（批量存成一条规则） |
| `insert` | `row` `value`（CSV 行） | 插入行 |
| `delete` | `row` `count` | 删除行 |
| `replace` | `from` `to` `col` | 查找替换 |
| `drop` / `rename` | `name` / `from`,`to` | 删列 / 改名 |
| `insert-column` | `name` `value` **或** `formula`，`after` | 插入常量列或公式列 |
| `formula` | `name` `formula` | 追加公式列 |
| `copy` | `from=A1:B2` `to=B3` | 区域复制（A1 风格范围） |
| `split-column` | `col` `by` `into` | 一列拆成多列（最后一片吃下剩余部分） |
| `fill` | `col` `from-row` `to-row` `value` | 区间填充（1-based，闭区间）；精确 `set` 优先于填充 |
| `undo` / `redo` | — | 撤销最后一条 / 重做 |
| `export` | `out` | 物化到文件（返回 job） |

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
## 已知限制

- `where` / `order` 走 DuckDB：对**有编辑的文件**会先物化到临时表（每份源文件最多一份，
  日志变化即替换），超过 **2 GB** 直接报错让你先 `edit export`。
- 单次结果封顶：`/query` 最多 1 万行、`/rows` 单页最多 `MAX_ROWS_PER_REQUEST` 行。
- `GET /layout` 带参数即写入，是唯一"读端点会写"的地方（为了少一个端点，代价是
  语义不纯；用 `POST` 也可以）。
