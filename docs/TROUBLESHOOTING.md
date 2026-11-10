# 排错 / Troubleshooting

「它说了什么」→「为什么」→「怎么办」。**每一节的引文都能被测试复现**，
所以这份文档不会随着代码漂移（`troubleshooting_quotes_are_real` 会红）。

## 找不到列 / column not found

```
✗ filter: column 'amounts' not found; did you mean 'amount'
  hint: run `csv-splitter analyze FILE` to see the exact column names
```

列名要**完全一致**（大小写、空格、全半角）。有编辑时，可用列是布局后的列名——
`edit rename` 之后旧名字就不存在了。`analyze` 或 `preview` 会打出真实表头。

## 公式报错 / a formula names something that does not exist

```
error: formula reads unknown column(s): nope (available: id, city, amount)
error: referenced sheet(s) not found next to t.csv: customers.csv
hint: ref() takes a file name in the same directory, e.g. ref("customers.csv", "cust_no", "name")
```

公式在**写进 journal 时**就校验：不确定的列名/表名当场拒绝，不等导出时才炸。
`ref()` 的三个参数都是**字符串**，写 `ref("x.csv", "id", "amount")`，不是 `ref("x.csv", id, amount)`。

## 文件打不开 / the file cannot be opened

```
✗ preview: No such file or directory (os error 2)
  hint: check the path; `csv-splitter files --root DIR` lists what is readable
```

`serve` 的四种路径错误各不相同，因为它们是四种不同的错：

| 消息 | 意思 |
|---|---|
| `path is required` | 没传 path |
| `no such file inside the served directory` | 文件名不存在 |
| `path is a directory, not a file` | 传来一个目录 |
| `path is outside the served directory` | 越界 |

## 索引 / index problems

```
✗ index: utf16.csv is utf-16le; the row index works on byte offsets — run `convert` to make it UTF-8 first
  hint: convert it first: csv-splitter convert FILE -o utf8.csv
✓ 0 data rows indexed …
```

UTF-16 的字节偏移不可靠，所以拒绝建索引；先 `convert` 成 UTF-8。
「0 data rows」通常意味着文件只有表头，或者**行尾不是 `\n`**（老 Mac 的 `\r`）。

索引文件坏了不用管：校验不通过就自动重建。

## 字段看起来错位 / fields look shifted

```
note: this file looks like it escapes quotes with a backslash (backslash); if fields look wrong, re-run with `--quotes backslash`
```

自动检测只给提示，不擅自改变解析。确认后加 `--quotes backslash`（或 `rfc4180` / `none`）。
`sort` / `concat` / `sql` 目前不接受这个参数（见 `CAPABILITIES.md`）。

## 看不到自己的编辑 / my edits are missing

```
note: 1 pending edit(s) in t.csv.journal are not applied by this command; run `csv-splitter edit export t.csv` to materialise them
```

两类命令读**文件**而不是表：`analyze` / `stats` / `search` / `sort` / `dedupe` / `concat` /
`columns` / `split` / `partition` / `lookup`。`groupby` / `pivot` / `chart` 可以加
`--apply-edits` 临时物化。要看套用编辑的表用 `view`，要交付用 `edit export`。

## 编辑丢了 / an edit vanished

```
warning: 1 record(s) in t.csv.journal could not be read and were ignored (a journal cut off mid-write looks like this)
```

journal 是逐行追加的，中断只损失最后一条。看到这条说明确实丢了一条，
其余编辑仍然有效。没有这条警告却少了编辑，那是别的原因（比如改的是不存在的行）。

## 覆写保护 / refusing to write over the input

```
✗ output: refusing to write over the input file t.csv (it is being read right now)
hint: choose a different -o path; the input is never modified by design
```

`-o` 指向正在读的文件会被拒绝——那是数据丢失。换个路径；覆盖**别的**文件是允许的，
只会提示 `note: overwriting existing out.csv`。

## 上限报错 / a limit was reached

```
✗ dedupe: more than 1 distinct rows; raise --max-keys or dedupe by column
✗ pivot: more than 5 distinct values in 'city'; raise --max-columns or pivot on a lower-cardinality column
✗ groupby: more than 5 groups; raise --max-groups or group by a lower-cardinality column
✗ chart: more than 5 categories in 'city'; raise --max-points or chart a lower-cardinality column
```

**这些从不静默截断**：超限要么报错，要么在结果里标 `truncated`（API 的 `truncated: true`、
`sql` 的 `(truncated)`、`/rows` 的 `count`/`requested`/`limit` 三个字段）。

## 行号 / row numbers are 1-based

```
error: rows are 1-based; row 1 is the first data row
```

`--row 0` 不存在。所有对外行号（`--row`、`--start`、`/rows?start=`、违规行的 `row`）
都是 1-based；只有 journal 文件内部是 0-based，那是实现细节。

## 慢 / it feels slow

| 现象 | 原因 | 办法 |
|---|---|---|
| 第一次打开大文件要等 | 在建索引 | 提前跑 `index`；`/sheet` 的 `indexed` 告诉你有没有 |
| 带 `--where` 的第二页慢一次 | 在物化整个结果集 | 之后翻页走缓存（60 MB 实测 18 ms）|
| `sort` / `partition` 吃内存 | 排序缓冲 | 调小 `--buf-mb` |
| `dedupe` 吃内存 | 要记住每个键 | 去掉 `--by` 前先想想基数；或提高 `--max-keys` 并接受内存 |

内存数字与「哪些命令会随行数增长」写在 `CAPABILITIES.md` 的实测表里。
