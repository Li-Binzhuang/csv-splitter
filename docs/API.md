# HTTP API

`csv-splitter serve --root DIR [--addr 127.0.0.1:8787]`

服务绑定回环地址，只读 `--root` 下的文件（路径经过 `canonicalize`，越界直接拒绝），
开启 CORS。除 `POST` 之外全是 `GET`；`GET /` 返回内置浏览页面。

## 约定 / Conventions

**错误统一形状**（4xx）：

```json
{"error":"by is required","parameter":"by","hint":"pass by=<column name>"}
```

`parameter` 指向出问题的参数，`hint` 说明该传什么。用法错误是 400，数据/引擎问题
是 422，未知路径是 404（响应里带 `endpoints` 与 `writes` 两个清单）。

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
| `GET /rows` | `path` `start` `count` `where` `order` `desc` `sql` `format` | 分页窗口；`where`/`order` 走 SQL，**会套用编辑** |
| `GET /sheet` | `path` | 列、行数、编辑数、能力清单、列布局、冻结列 |
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
| `GET /search` | `path` `q` `columns` | 跨列搜索 |
| `GET /files` | `ext` `limit` | 根目录下可读文件清单 |
| `GET /download` | `path` | 原样下载文件 |
| `GET /export` | `path` `sql` `where` | 导出（查询结果或筛选结果）为 CSV |
| `GET /query` | `path` `sql` `limit` | 一次性 SQL 结果（不分页） |
| `GET /job` | `id` | 任务状态：`state` `phase` `fraction` `output` `error` |
| `GET /jobs` | — | 最近的任务列表 |

## 写入 / Writes

| 端点 | 参数 | 说明 |
|---|---|---|
| `POST /edit` | `path` `action` … | `set`（`row` 或 `where`）、`insert`、`delete`、`replace`、`drop`、`rename`、`insert-column`、`copy`、`formula`、`undo`、`export` |
| `POST /split` | `path` `splits` `out` `format` `threads` | 切分（`splits` 为片数） |
| `POST /merge` | `out` `format` | 合并（根目录下的分片） |
| `POST /sort` | `path` `by` `desc` `out` | 外部排序 |
| `POST /dedupe` | `path` `by` `out` | 去重 |
| `POST /partition` | `path` `by` `out` `format` `maxPartitions` | 按列值拆成多个文件 |
| `POST /warm` | `path` `sql` `start` `count` | 预热查询缓存 |

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
| `undo` | — | 撤销最后一条 |
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

## 已知限制

- `where` / `order` 走 DuckDB：对**有编辑的文件**会先物化到临时表（每份源文件最多一份，
  日志变化即替换），超过 **2 GB** 直接报错让你先 `edit export`。
- 单次结果封顶：`/query` 最多 1 万行、`/rows` 单页最多 `MAX_ROWS_PER_REQUEST` 行。
- `GET /layout` 带参数即写入，是唯一"读端点会写"的地方（为了少一个端点，代价是
  语义不纯；用 `POST` 也可以）。
