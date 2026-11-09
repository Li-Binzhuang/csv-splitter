# 10 分钟上手 / Getting started in 10 minutes

这份引导用一份**故意不干净**的示例文件走完整条链路。每一步都贴了真实输出，
你可以照着跑一遍，也可以对照自己的数据看结果对不对。

> 为什么示例是脏的：干净的示例会教你错误预期。真实 CSV 有重复行、空单元格、
> 带引号的换行、以及多一个逗号的行。工具在这些情况下怎么办，才是你要知道的。

---

## 0 · 生成示例文件

```bash
$ csv-splitter demo
✓ wrote csv-splitter-demo/orders.csv (20120 data rows, 918.0 KB)
```

文件在 `./csv-splitter-demo/orders.csv`，6 列：`id,cust_no,city,amount,note,ts`，
2 万行，其中 20 行有额外逗号、100 行重复、900 个空备注。

```bash
cd csv-splitter-demo   # 后面的命令都在这个目录里跑
```

## 1 · 看一眼：`preview`

```bash
$ csv-splitter preview orders.csv --rows 5
orders.csv — 0.9 MB, ~20499 rows, delimiter Comma
id | cust_no | city | amount | note | ts
----------------------------------------
0 | C042989 | Puebla | 2639.00 |  | 2026-08-15
1 | C049574 | Puebla | 76330.00 | first line
second line | 2026-03-10
2 | C035030 | Puebla | 10804.00 | amounts: 1,200.00 and 3,400.50 | 2026-06-22
```

**注意第 2 行**：`note` 字段里有真实换行，显示时跨了两行——但它**仍然是一条记录**。
如果按物理行数切分，这条记录会被切成两半，后面的列全部错位。

**注意 `2` 那一行的金额**：`amounts: 1,200.00 and 3,400.50` 里有两个逗号，
因为加了引号，所以仍然是一个字段。不加引号的话就是两个字段——第 20 行起就是这种情况。

## 2 · 摸清数据：`analyze`

```bash
$ csv-splitter analyze orders.csv
⚠ 20 of 20120 rows have a field count different from the header — columns may be shifted on those rows
column                    rows    empty  distinct  summary
id                       20120        0     20000  min 0.00  max 19999.00  mean 9973.91  median 9956.00  sum 200675140.00
city                     20120        0         5  text: Guadalajara (4073), Puebla (4038), CDMX (4017)
amount                   20120        0     17896  min 103.00  max 89998.00  mean 44798.96  median 44427.00  sum 901355120.00
note                     20120      900         4  text: ok (17600), amounts: 1,200.00 and 3,400.50 (800), first line…
```

三条能立刻用的信息：

1. **第一行警告**是最重要的：20 行字段数不对 → 这 20 行的列是**错位的**
2. `city` 只有 5 个不同值 → 适合做分组维度
3. `amount` 是数值列（min/max/mean/median/sum 都有），`note` 有 900 个空值

## 3 · 找某个值：`search`

```bash
$ csv-splitter search orders.csv "second line" --limit 3 --json
{"scanned":20120,"matched":800,"truncated":true,
 "columns":["id","cust_no","city","amount","note","ts"],"rows":[3,29,54]}
```

`matched` 是全部命中数，`rows` 是**行号**（从 1 开始，表头不算）。
有了行号就能直接跳过去看：

```bash
$ csv-splitter preview orders.csv --start-row 2 --rows 1
```

## 4 · 筛出来存成文件：`filter --out --verify`

```bash
$ csv-splitter filter orders.csv --where "city=CDMX" --where "amount>1000" -o cdmx.csv --verify
✓ 3978 of 20120 rows matched → cdmx.csv (173085 bytes)
  ✓ checked: 3978 rows, 6 columns
```

- 重复 `--where` = AND；`--any` 变成 OR
- `--verify` 让工具**回头读一遍结果**，确认行数、列数都对得上

更省事的写法：

```bash
# 枚举
csv-splitter filter orders.csv --where "city in CDMX,Puebla" -o两城.csv
# 正则
csv-splitter filter orders.csv --where "note~re:^first" -o multi.csv
```

## 5 · 去重：`dedupe`

```bash
$ csv-splitter dedupe orders.csv -o deduped.csv --verify
✓ 20020 of 20120 rows kept, 100 removed → deduped.csv (913.9 KB)
  ✓ checked: 20020 rows, 6 columns
```

`20020 + 100 = 20120`，一行等式证明没丢行。按列去重加 `--by cust_no`。

## 6 · 分组汇总：`groupby`

```bash
$ csv-splitter groupby orders.csv --by city --agg "amount:sum,amount:count" --order sum_amount --desc
city,sum_amount,count_amount
Guadalajara,182878367,4073
CDMX,181344992,4017
Monterrey,180567858,4017
Puebla,179700480,4038
Tijuana,176863423,3975
```

聚合算子：`count` / `sum` / `mean` / `min` / `max`，可多列、可多 key。
内存只跟**分组数**有关，不跟行数有关。

## 7 · 整理列再发出去：`columns`

```bash
$ csv-splitter columns orders.csv --drop note --rename "city:城市" --verify
✓ 20120 rows × 5 columns → orders.reshaped.csv (819.9 KB)
    columns: id, cust_no, 城市, amount, ts
    dropped: note
  ✓ checked: 20120 rows, 5 columns
```

## 8 · 拆开、拼回、证明无损

```bash
$ csv-splitter split orders.csv -n 4 -f none -o parts.tar
$ csv-splitter merge parts.tar -o restored.csv
```

`verify` 会自动做一遍「拆 → 合 → 逐字节比较」：

```bash
$ csv-splitter verify orders.csv -n 4
✓ lossless: 939996 bytes across 4 chunk(s) round-tripped identically
```

任何切分方案都可以这样证明无损，不需要信我们的话。

## 9 · 给前端用：`serve`

```bash
$ csv-splitter serve --root . --addr 127.0.0.1:8787
csv-splitter API on http://127.0.0.1:8787  (root: …)
```

然后在浏览器打开 **http://127.0.0.1:8787/** —— 有一个内置的单文件页面，
可以浏览文件、翻页、筛选、搜索、看列画像、跑分组汇总。它只用公开端点，
可以当作前端接手的参考实现。

也可以直接用 curl：

```bash
$ curl "http://127.0.0.1:8787/files?ext=csv"
{"root":"…/csv-splitter-demo","truncated":false,"files":[{"path":"orders.csv","bytes":939996,…}]}
$ curl "http://127.0.0.1:8787/meta?path=orders.csv"
$ curl "http://127.0.0.1:8787/rows?path=orders.csv&start=0&count=20"
$ curl "http://127.0.0.1:8787/groupby?path=orders.csv&by=city&agg=amount:sum"
```

只绑 127.0.0.1，只读 `--root` 之内的文件，`../` 一律 400。所有接口见
[README 的 HTTP API 一节](../README.md)。

---

## 完整命令一览

| 想做的事 | 命令 |
|---|---|
| 生成示例 | `demo` |
| 有什么文件 | `files --root DIR` |
| 打开看一眼 | `preview FILE --rows N` |
| 跳到某一行 | `preview FILE --start-row N` |
| 摸清每列 | `analyze FILE` |
| 找值 | `search FILE TEXT` |
| 筛（AND/OR/枚举/正则） | `filter FILE --where … [--any] [-o OUT] [--verify]` |
| 去重 | `dedupe FILE [--by A,B] -o OUT [--verify]` |
| 排序 | `sort FILE --by "a:asc,b:desc" -o OUT` |
| 分组汇总 | `groupby FILE --by K --agg "c:op,…" -o OUT` |
| 改列 | `columns FILE --keep/--drop/--rename … -o OUT [--verify]` |
| 按值拆分 | `partition FILE --by COL -o OUT.tar` |
| 拼接 | `concat A.csv B.csv -o OUT.csv` |
| 切分 / 压缩 | `split FILE -n N [-f zst] [-t 4]` |
| 合并 | `merge ARCHIVE -o OUT.csv` |
| 无损自检 | `verify FILE -n N` |
| 换编码 | `convert FILE --from gbk --to utf-8` |
| SQL | `sql FILE "SELECT …"` |
| 本地 API | `serve --root DIR` |
