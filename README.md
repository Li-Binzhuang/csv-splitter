# CSV Splitter Pro

> Cross-platform CSV splitter, compressor, previewer & column statistics tool with bilingual (Chinese/English) UI.
>
> 跨平台 CSV 文件拆分、压缩、预览与列统计工具，支持中英文界面。
>
> **刚上手？** 跑一句 `csv-splitter demo` 生成示例文件，然后看 [10 分钟上手](docs/GETTING_STARTED.md)。

[![CI](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/ci.yml/badge.svg)](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/ci.yml)
[![Release](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/release.yml/badge.svg)](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/release.yml)
[![GitHub Release](https://img.shields.io/github/v/release/Li-Binzhuang/csv-splitter?include_prereleases&sort=semver)](https://github.com/Li-Binzhuang/csv-splitter/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

---

## Quick start / 快速开始

```bash
$ csv-splitter demo            # 生成一份带脏数据的示例文件
$ cd csv-splitter-demo
$ csv-splitter preview orders.csv
$ csv-splitter analyze orders.csv
$ csv-splitter groupby orders.csv --by city --agg "amount:sum,amount:count" --order sum_amount --desc
$ csv-splitter serve --root .  # 本地 JSON API，打开 http://127.0.0.1:8787/ 就能看
```

每一步的真实输出、以及从筛选到交付的完整流程，见 **[10 分钟上手](docs/GETTING_STARTED.md)**。

## Download / 下载

从 [Releases](https://github.com/Li-Binzhuang/csv-splitter/releases/latest) 页面下载对应平台的预编译包：

| Platform | Artifact | Arch |
|----------|----------|------|
| 🐧 Linux | `csv-splitter-linux-x86_64.tar.gz` | x86_64 |
| 🍎 macOS | `csv-splitter-macos-arm64.tar.gz` | Apple Silicon |
| 🍎 macOS | `csv-splitter-macos-x86_64.tar.gz` | Intel |
| 🪟 Windows | `csv-splitter-windows-x86_64.zip` | x86_64 |

> 每个包附带 `.sha256` 校验文件，可用于验证下载完整性。

## Features / 功能

| Feature | Description | 说明 |
|---------|-------------|------|
| 🔪 Split & Compress | Split large CSV into compressed chunks (Zstd/Gzip/Bzip2/XZ/Zip) | 将大型 CSV 拆分为多个压缩分片 |
| 🔄 Toggle Pipeline | Independently enable/disable split and compress steps | 可独立开关拆分与压缩步骤 |
| 📋 Column Filter | Select columns from CSV headers or via filter file | 通过表头选择或过滤文件筛选列 |
| 🔃 Sort | External merge sort by column with numeric-aware ordering | 按指定列外部归并排序，支持数值智能排序 |
| 👁 Data Preview | Preview CSV rows with auto-detected delimiter, row/col range & freeze | 预览 CSV 数据，支持行列范围与冻结 |
| ⚡ Row Index | 8 bytes per 1000 rows sidecar, jump to any row without scanning | 稀疏行索引：跳转到任意行，无需从头扫描 |
| 📊 Column Stats | Reservoir sampling analysis (unique/empty/top frequency) | 蓄水池采样分析列数据分布 |
| 📦 Decompress | Supports Zstd, Gzip, Bzip2, XZ, Zip, 7z, Tar and combos | 支持 12 种压缩格式及其组合 |
| 🌐 i18n | Chinese (default) / English, one-click switch | 默认中文，一键切换英文 |
| 📁 Drag & Drop | Drop files onto cards, shows file metadata | 拖放文件到卡片区域，显示文件元数据 |
| 🎞 Flow Visualization | Animated pipeline: Source → Split → Compress → Result | 动画流程图，动态显示活跃步骤 |
| 🧩 Merge | Re-assemble chunks back into one CSV (inverse of split) | 把分片重新合并回单个 CSV（拆分的逆操作） |
| ✅ Verify | Split → merge round-trip check that proves a split is lossless | 拆分后合并做字节级校验，证明拆分无损 |
| 💻 CLI | Full headless interface for cron / CI / servers, no GUI needed | 完整命令行，适合 cron / CI / 服务器 |
| 🌐 HTTP API | JSON API for a web front-end: browse, search, filter, aggregate, split | 供前端的 JSON 接口：浏览、搜索、筛选、聚合、切分 |
| 🔍 Search | Find text anywhere, or in chosen columns; returns row numbers | 跨列搜索，返回行号（可用 `--start-row` 跳过去） |

## Roadmap / 路线图

当前进度、北极星指标与后续里程碑见 [docs/ROADMAP.md](docs/ROADMAP.md)。

## Screenshots / 截图

### 🔪 拆分压缩 / Split & Compress

![Split & Compress](docs/split.png)

### 👁 数据预览 / Data Preview

![Data Preview](docs/preview.png)

### 📊 列统计 / Column Statistics

![Column Statistics](docs/stats.png)

### 📦 解压缩 / Extract

![Extract](docs/extract.png)

## CLI / 命令行

桌面版适合手动处理，服务器和脚本需要的是无界面入口。`crates/cli` 与 GUI 共用同一套核心管线，
行为不会漂移。

```bash
# 安装到 ~/.cargo/bin
cargo install --path crates/cli

# 拆分（默认 zstd、10 片）
csv-splitter split big.csv -n 8 -f zst -o chunks.tar

# 按目标大小切：每片约 100 MB（片数由数据量决定）
csv-splitter split huge.csv --by-size 100MB -f zip -o huge.zip

# 按列值分区：一个城市一个文件
csv-splitter split orders.csv --by-column city -f zst

# 只要几列，并在拆分前按列排序
csv-splitter split big.csv --columns id,phone,score --sort score --desc -n 4 -f zip

# 合并回单文件（支持 tar / tar.gz / tar.zst / tar.bz2 / tar.xz / zip）
csv-splitter merge chunks.tar -o restored.csv

# 证明这次拆分是无损的：split → merge → 逐字节比较
csv-splitter verify big.csv -n 6

# 建立行索引（1.6M 行 → 13 KB 索引，0.5 秒）
csv-splitter index big.csv

# 跳到任意行（有索引时是微秒级）
csv-splitter preview big.csv --start-row 1500000 --rows 20

# 筛选：返回行号，再用 preview --start-row 跳过去看
csv-splitter filter orders.csv --where "city=CDMX"
csv-splitter filter orders.csv --where "amount>1000" --json

# 看一眼数据与列分布
csv-splitter preview big.csv --rows 5
csv-splitter stats big.csv --sample 50000 --top 5

# 解压任意支持的归档
csv-splitter extract chunks.tar -o ./out
```

所有子命令都支持 `-d/--delim auto|comma|tab|semicolon|pipe|<char>`、`-q/--quiet`、`-h/--help`。

### 设计上的三个保证 / Three guarantees

1. **三种切法**：`-n/--splits` 按片数均分（片差 ≤ 1 条记录）；`--by-size` 按目标大小切
   （片数由 `ceil(数据量 / 目标)` 决定，单片最多超出目标一条记录）；`--by-column` 按某列的
   取值分区，一个值一个文件（空值落 `_empty`，文件名自动清洗，值过多会报警而不是生成上千个文件）。
2. **无损**：`split` 按*逻辑记录*切分（引号内换行的字段算一条记录），`merge` 去掉重复表头后按序拼接，
   `verify` 用字节比较兜底。`splits > 记录数` 时不会产生空分片。
3. **内存可控**：`--buf-mb` 是**总预算**，拆成 N 片不会变成 N × 缓冲。旧版每片固定 64 MB，
   10 片时峰值接近 700 MB。

## 基础能力 / Basics

一个 CSV 工具首先得能「打开、看见、找到、拿出去」。对应的最小命令集：

| 想做的事 | 命令行 | HTTP |
|---|---|---|
| 看看有什么文件 | `files --root ~/data` | `GET /files?ext=csv` |
| 打开看一眼 | `preview big.csv --rows 20` | `GET /meta` + `GET /rows` |
| 跳到某一行 | `preview big.csv --start-row 1500000` | `GET /rows?start=N` |
| 找某个值（跨所有列） | `search big.csv refund` | `GET /search?q=refund` |
| 按列筛选 | `filter big.csv --where "city=CDMX"` | `GET /filter?where=city=CDMX` |
| **筛完直接存文件** | `filter ... -o out.csv` | `GET /export?where=...` |
| **整表排序** | `sort big.csv --by amount -o sorted.csv` | `POST /sort` |
| 统计列分布 | `stats big.csv --sample 50000` | `GET /stats?sample=50000` |
| **摸清一份陌生数据** | `analyze big.csv` | `GET /analyze` |
| **去重** | `dedupe big.csv --by id -o clean.csv` | `POST /dedupe` |
| **整理列再发出去** | `columns big.csv --drop id --rename "city:城市"` | — |
| **按某列拆成多个文件** | `partition big.csv --by city` | `POST /partition` |
| **多个同构文件拼成一张表** | `concat a.csv b.csv c.csv -o all.csv` | — |
| **分组汇总** | `groupby big.csv --by city --agg "amount:sum"` | `GET /groupby` |
| 跑聚合 / 排序 | `sql big.csv "SELECT ..."` | `GET /query?sql=...` |
| 切分 / 合并 | `split` / `merge` | `POST /split` · `POST /merge` |
| 换编码 | `convert gbk.csv -o utf8.csv` | — |
| 把文件给出去 | — | `GET /download?path=`（原始字节） |
| 把查询结果拿出去 | — | `GET /export?sql=...`（CSV 文本） |
| 无损自检（字节级） | `verify big.csv -n 6` | — |
| **写操作自检（计数级）** | 写操作自检（计数级） | `filter … --verify` / `sort … --verify` / `partition … --verify` | — |

```bash
$ csv-splitter search orders.csv refund --show 3
scanned 200000 rows, matched 2 (2 shown)
rows (1-based, header excluded): 1, 3

        1  1 | CDMX | refund issued
        3  3 | CDMX | pay refund later
```

### 认识一份数据

```bash
$ csv-splitter analyze orders.csv
column                    rows    empty  distinct  summary
id                      200000        0  ≥100000  min 0.00  max 199999.00  mean 99999.50  median 99760.00  sum 19999900000.00
city                    200000        0         5  text: Tijuana (40189), CDMX (40116), Guadalajara (39987)
note                    200000    40171         4  text: ok (40370), 多行备注 (40034), quote"inside (39797)
```

一个流式 pass 出全部结论：空值数、去重数、数值列的 min/max/均值/中位数/求和、
文本列的长度极值与高频值。**数值列必须有全部非空值都是数字**才算数值列——
一个 `N/A` 就会让它退回文本列并标注 `mixed`。去重数超过上限会显示 `≥N`，
而不是假装自己精确。

```bash
$ csv-splitter dedupe orders.csv --by cust_no -o clean.csv
✓ 49124 of 200000 rows kept, 150876 removed → clean.csv (3.0 MB)
```

### 分组汇总

```bash
$ csv-splitter groupby orders.csv --by city --agg "amount:sum,amount:mean,amount:count" --order sum_amount --desc
✓ 200000 rows → 5 groups → orders.grouped.csv (268 B)

city,sum_amount,mean_amount,count_amount
Tijuana,1812328212.7,45095.13,40189
Puebla,1803568070.11,45219.21,39885
```

聚合算子：`count` / `sum` / `mean` / `min` / `max`，可多列、可多 key。
内存只跟**分组数**有关，不跟行数有关；超过 `--max-groups`（默认 100 万）会
报错而不是把内存吃满——按 id 分组基本总是误操作。

求和用了补偿求和（Kahan），否则 4 万行累加后在末位会看出偏差。

文本列的 `sum`/`mean` 输出 `<mixed>` 而不是悄悄只算数字行；`min`/`max` 在
文本列上按字典序。

### 拆分与拼接

```bash
# 每个 city 一个文件，打包成 tar（不按大小切，按值切）
$ csv-splitter partition orders.csv --by city -o per_city.tar
✓ partitioned by city → per_city.tar (12.1 MB)

# 多个月的导出拼成一张表；表头会自动核对
$ csv-splitter concat march.csv april.csv may.csv -o q2.csv
✓ 3 files, 2100000 rows → q2.csv (280 MB)
```

`partition` 内部先按该列做外部排序（等值聚到一起，一次只开一个写句柄），
所以高基数列不会撞上文件句柄上限；超过 `--max-partitions`（默认 512）会拒绝，
因为按 `id` 分区基本总是误操作。

`concat` **默认核对表头**：列名不一致直接报错并指出是哪个文件。想强行拼接加
`--ignore-header`——它会拼，但会把不一致的文件列出来。

### 整理列

```bash
$ csv-splitter columns orders.csv --drop internal_id --rename "city:城市,amount:金额" -o share.csv
✓ 200000 rows × 5 columns → share.csv (9.8 MB)
    columns: id, cust_no, 城市, 金额, ts
    dropped: note
```

`--keep a,b` 只保留并重排，`--drop a,b` 删掉，`--rename old:new` 改名。
既要保留又要删同一列会**直接报错**，而不是让某一方悄悄赢。

和 `filter --out` 不同，这里**必须重新序列化**（列集合变了）：含逗号、引号、
换行的单元格会按 CSV 规则重新加引号，多行字段进去是一条记录、出来也是一条。

按整行去重（默认）或按 `--by` 指定的列去重，保留**首次出现**的那条。
内存只存 128 位哈希（每条 16 字节），1000 万条约 160 MB；超过 `--max-keys`
会**报错而不是悄悄丢行**。

### 筛选与排序

```bash
# 两个条件 = AND；重复 --where 即可
csv-splitter filter orders.csv --where "city=CDMX" --where "amount>45000" -o cdmx.csv
# ✓ 19836 of 200000 rows matched → cdmx.csv (1195614 bytes)

# --any 改成 OR
csv-splitter filter orders.csv --where "city=Tijuana" --where "amount>49990" --any -o any.csv

# 多列排序，逐列指定方向
csv-splitter sort orders.csv --by "city:asc,amount:desc" -o sorted.csv
# 单列仍是最简单的写法
csv-splitter sort orders.csv --by amount --desc -o sorted.csv

# 枚举几个值
csv-splitter filter orders.csv --where "city in CDMX,Puebla,Tijuana" -o in.csv

# 正则筛选（col~re: 前缀），可与其他条件用 AND / OR 组合
csv-splitter filter orders.csv --where "note~re:^REFUND" --where "amount>1000" -o hits.csv
```

`-o` 是「筛完留下来」的路径：流式写出，命中行的**原始字节原样保留**（不重新序列化，
引号风格和换行都不变）。不带 `-o` 时仍然只列出行号，方便跳转查看。

对方便前端的那一侧：`GET /export?where=city%3DCDMX|amount%3E100` 直接返回筛选后的 CSV；
`POST /sort?by=amount&desc=1&out=sorted.csv` 起一个后台排序任务。

搜索默认**不区分大小写、跨所有列做子串匹配**；`--columns city` 收窄范围，
`--case` 区分大小写，`--exact` 要求整格相等（`--case`/`--exact` 可以和 `--columns` 组合）。

## 编码 / Encoding

自动识别 **UTF-8 / UTF-8 BOM / GBK / UTF-16 LE / UTF-16 BE**：

- 预览、列统计、切分（含列过滤）、API 都按识别结果解码，中文不再乱码
- UTF-8 BOM 会被剥离，不会污染第一个列名
- 行索引基于字节偏移，适用于 **UTF-8 与 GBK**（两者的 `,` 和换行都是 ASCII，不会出现在多字节字符里）；
  UTF-16 的换行是两字节，随机访问会明确报错而不是给出错位数据
- 需要随机访问 UTF-16 文件时先转码，之后就是一个普通 UTF-8 CSV：

```bash
csv-splitter convert export.csv -o export.utf8.csv     # 自动识别来源编码
csv-splitter index export.utf8.csv
```

### 内置页面 / Built-in viewer

`serve` 起来后直接打开 `http://127.0.0.1:8787/`，有一个单文件 HTML 页面：

- 文件列表（来自 `/files`，只列根目录内的文件）
- 行浏览：翻页、跳行、按条件筛选、跨列搜索
- 列画像：一键跑 `/analyze`，ragged 行会红字提示
- 分组汇总：填分组列和聚合表达式

它只用公开端点（`/files` `/meta` `/rows` `/search` `/analyze` `/groupby`），
**不做任何私有通道**——所以它能做到的，任何真正的前端都能做到；它也能当成
「后端做完了」的可视化证明，以及前端接手的参考实现。

### 查询结果缓存 / Result cache

滚动不该每次都重扫文件。`/rows?sql=` 第一次执行会把结果物化（默认上限 20 万行、
总预算 40 万行），之后翻页直接切片：

```bash
# 第一页：真正跑查询
time curl ".../rows?path=big1g.csv&sql=SELECT city,amount FROM t ORDER BY amount DESC&start=0&count=20"
# 0.70s   {"start":0,"hasMore":true,"rowsSoFar":200000,...}

# 第二页：命中缓存
time curl "...&start=20&count=20"
# 0.00s   {"start":20,"hasMore":true,"rowsSoFar":200000,...}
```

**预热**：含引号换行的文件会回退到串行扫描，10 GB 上首次查询要几秒。
客户端一确定筛选条件就先 `POST /warm`，用户开始滚动时缓存已经就绪：

```bash
curl -X POST "http://127.0.0.1:8787/warm?path=big1g.csv&sql=SELECT city,amount FROM t ORDER BY amount DESC"
# {"job":"job1a1c094da0d0"}     然后轮询 /job?id=...

# 预热完成后再取第一页
# 0.03s  {"start":0,"hasMore":true,"rowsSoFar":200000,...}
```

已缓存时 `/warm` 返回 `200 {"cached":true}` 而不是再起一个任务；非法 SQL 在起任务前就 400。

语义上诚实的三点：

- 结果超过上限时只物化前缀，`rowsSoFar` 告诉你缓存里有多少行；
  请求落在前缀内照常切片，**越过前缀则返回 422 让你收窄查询**，而不是假装结果就这么多
- 文件长度或 mtime 变了就失效
- 预算超了按 LRU 淘汰；超限的结果不进缓存（避免把「不完整」当「完整」）

### 长任务 / Long-running jobs

拆分、合并这类动辄几分钟的操作不占连接：POST 提交，立刻拿到 job id，之后轮询。

```bash
# 提交
curl -X POST "http://127.0.0.1:8787/split?path=big.csv&splits=8&format=zst&out=parts.tar"
# => {"job":"job1a2b3c4d"}

# 轮询（fraction 0..1，elapsed 秒，messages 是最近 20 条阶段信息）
curl "http://127.0.0.1:8787/job?id=job1a2b3c4d"
# => {"state":"running","phase":"Splitting & compressing","progress":52428800,"total":134217728,"fraction":0.391,...}

curl "http://127.0.0.1:8787/jobs"     # 最近 50 个任务
```

- 输出路径同样受 `--root` 约束：`out=../../tmp/x.tar` 返回 400
- 任务跑在自己线程上，客户端断开不影响它
- 完成的任务保留最近 50 个，运行中的不会被回收

## SQL / 分析

手写 GROUP BY、窗口函数、类型推断是这类工具最终烂掉的原因。这里把 **DuckDB** 接进来——
它原生读 CSV、并行扫描、有真正的优化器，我们只做门口的一道检查。

```bash
csv-splitter sql orders.csv "SELECT city, count(*) n, round(avg(amount),1) q FROM t GROUP BY 1 ORDER BY n DESC"
csv-splitter sql orders.csv "SELECT * FROM t" --limit 5 --json
```

```bash
curl "http://127.0.0.1:8787/query?path=orders.csv&sql=SELECT%20city,count(*)%20FROM%20t%20GROUP%20BY%201"
```

**规则**（因为这是暴露给 HTTP 的）：

- 只允许单条 `SELECT` / `WITH`
- 拒绝 `INSERT/UPDATE/DELETE/DROP/CREATE/ALTER/ATTACH/COPY/EXPORT/INSTALL/PRAGMA...`
- 拒绝 `read_csv_auto()` 之类的文件读取函数——数据只能来自 `--root` 白名单内那个文件
- 结果默认上限 1000 行（`--limit` 可调，最高 10 万），`truncated` 字段告知是否被截断
- 文件以视图 `t` 注册，查询里不需要写路径

**实测**（20 万行 CSV，4 个城市分组求和）：

```
city | n | avg_amt
Tijuana | 50127 | 44916.4
CDMX    | 50069 | 45221.4
...
4 row(s) in 0.67s
```

> DuckDB 已随二进制静态编译（`bundled`），无需安装。它是可选 feature `sql`，不使用时可以关掉。

## HTTP API / 后端接口

前后端分离时的后端：一个只读 JSON API，浏览器只请求「当前屏幕上的那几十行」，
文件再大也不进内存。

```bash
csv-splitter serve --root ~/data --addr 127.0.0.1:8787
```

| 端点 | 说明 |
|------|------|
| `GET /health` | 存活检查 |
| `GET /meta?path=REL` | 行数、字节数、表头、分隔符、编码 |
| `GET /rows?path=REL&start=N&count=M` | 任意行窗口（count ≤ 2000） |
| `GET /rows?path=REL&sql=...&start=N&count=M` | **查询结果的第 K 页**（结果物化一次后走缓存，翻页 0 ms） |
| `GET /stats?path=REL&sample=N` | 列统计（唯一值 / 空值 / Top N） |
| `GET /filter?path=REL&where=col=value` | 全表筛选，返回命中的行号（1 起，不含表头） |
| `GET /query?path=REL&sql=SELECT...` | 对文件跑只读 SQL（DuckDB） |
| `POST /split?path=REL&splits=N&format=zst&out=REL` | 提交拆分任务，返回 job id |
| `POST /merge?path=REL&out=REL` | 提交合并任务，返回 job id |
| `GET /download?path=REL` | 原始字节（`Content-Disposition: attachment`） |
| `GET /export?path=REL&sql=...` | 查询结果作为 CSV 文本，可直接喂给别的工具 |
| `POST /warm?path=REL&sql=...` | 后台预热查询结果缓存（返回 job id） |
| `GET /job?id=...` / `GET /jobs` | 任务状态 / 近期任务列表 |

约束与安全：

- 只绑定回环地址（传入非回环地址会打印警告）
- 所有 `path` 都相对 `--root` 解析，`canonicalize` 后必须仍在根目录内，`../` 直接 400
- 已带 `Access-Control-Allow-Origin: *`，任意前端可直接跨域调用
- 无写操作；split/merge 的任务化 API 在路线图 M3

示例：

```bash
curl "http://127.0.0.1:8787/meta?path=huge.csv"
curl "http://127.0.0.1:8787/rows?path=huge.csv&start=18000000&count=20"
```

## 错误信息 / Error output

只有两种形状，脚本可以直接按前缀判断：

| 类型 | 退出码 | 形态 |
|---|---|---|
| 用法错误 | **2** | `error: <原因>`（stderr） |
| 运行时失败 | **1** | `✗ <命令>: <原因>`，下面可能有一行 `hint:` |

```
$ csv-splitter preview /definitely/not/here.csv
✗ preview: No such file or directory (os error 2)
  hint: check the path; `csv-splitter files --root DIR` lists what is readable

$ csv-splitter filter orders.csv
error: filter needs at least one --where <expr>
```

`hint` 只在**有明确下一步**时出现（编码不支持 → 告诉你用 `convert`；
列名找不到 → 告诉你用 `analyze` 看真实列名；分组太多 → 建议换列或调上限）。
不该有的提示语不如没有。

## 自检 / Self-checks

每个写操作都会对结果做一个承诺，`--verify` 让它**去验证自己的承诺**：

| 操作 | 承诺 | 怎么验 |
|---|---|---|
| `split` / `merge` | 字节一致 | `verify`（逐字节比较） |
| `filter -o` | 输出里正好是命中的那些行 | 重读输出，比对行数、列数、继承的 ragged 行数 |
| `columns` | 行数不变，正好是留下的那些列 | 同上 |
| `dedupe` | `kept + removed == scanned` | 先查算术，再重读输出查行数 |
| `sort` | 行数、列数不变，只是顺序变了 | 重读源和结果比对计数 |
| `partition` | 所有源行都落在某一个分区里 | 解包（含压缩成员）逐个数行，求和比对 |

```
$ csv-splitter filter messy.csv --where "城市=CDMX" -o f.csv --verify
✓ 7853 of 64176 rows matched → f.csv (372582 bytes)
  ✓ checked: 7853 rows, 6 columns
```

**为什么 ragged 行不算失败**：`filter` 是原样搬运，源文件本来就有的错位行会一起搬过去——
这不是它造的错。所以自检比对的是「结果 ragged 数 ≤ 源 ragged 数」，而不是「必须为 0」。
否则这个检查会在真实脏数据上永远报警，然后没人再看它。

代价是多读一遍文件，所以是可选的（`--verify`）。比起「我觉得应该没问题」，
多花一次扫描很值。

## 脏数据 / Dirty files

真实世界的 CSV 不是教科书里的样子。这一行是为了让工具**告诉你哪里脏**，
而不是悄悄算出一个错答案：

```bash
$ csv-splitter analyze messy.csv
⚠ 27474 of 64176 rows have a field count different from the header — columns may be shifted on those rows
column                    rows    empty  distinct  summary
id                       64176        0     50001  text (mixed with numbers): 备注 (9176), 1098 (2), ...
```

| 情况 | 现在的行为 |
|---|---|
| 表头带多余空格（`城市 `） | 按精确名找；找不到时**唯一**的 trim 匹配会被接受，多个匹配则报 ambiguous |
| 列名写错 | `column 'zzz' not found; available columns: ...` 或 `did you mean 'amount'`，而不是空结果 |
| 行长与表头不一致 | `analyze` 显示 ragged 行数；行本身按原样展示（不擅自补齐） |
| GBK / CRLF / BOM | 自动探测并解码，`/meta` 会报 `"encoding":"gbk"` |
| **Windows-1252**（西欧 ANSI） | 探测为 `windows-1252`，`café` 不会变成乱码 |
| 零字节 / 只有表头 | 不报错：`analyze` 说「没有列」，拆分产物合并后**字节一致** |
| 2 MB 的单行字段 | 正常处理，不整行读进内存之外的地方 |
| 引号内换行 | 一条记录；记录数与 Python `csv` 模块逐行比对一致 |

**编码约定**（重要）：只搬运字节的命令（`split`、`merge`、`sort`、`filter -o`）**保持源编码**；
改变列或值的命令（`columns`、`groupby`、`partition`、`dedupe`）输出 **UTF-8**。
混着用时请留意。

## 基准 / Benchmarks

```bash
cargo bench -p csv-splitter-core          # 输出真实数字（criterion）
cargo test -p csv-splitter-core --test performance   # 回归护栏，0.2 秒
```

criterion 只报告不判定——共享 CI 跑者的抖动会让硬阈值变成噪声源。
真正会让构建失败的是 `tests/performance.rs`，它的阈值刻意放宽约 50 倍，
只为拦住**病态退化**（误写成 O(n²)、丢了缓冲、每行一次系统调用）：

| 护栏 | 断言 |
|------|------|
| 索引构建 | > 20 MB/s（实测 ~2 GB/s 热缓存） |
| 随机访问 | 尾行绝对 < 100 ms，且不比首行慢 20 倍以上 |
| 拆分 | > 10 MB/s，产物大小 > 输入一半 |

本机 criterion 基线（500k 行 / 14 MB）：

```
index_build/100000    ~60 ms     (~230 MB/s 冷)
random_access/middle  ~39 µs     (20 行)
split/1               ~34 ms     (414 MiB/s)
split/4               ~47 ms     (299 MiB/s)
```

### 并行切分 / Parallel split

单线程读取的上限是磁盘顺序带宽（M1 Pro 实测约 490 MB/s）。大文件可以用多线程读：

```bash
csv-splitter split big.csv -n 8 --threads 4      # 或 -t 4
curl -X POST ".../split?path=big.csv&splits=8&threads=4&out=parts.tar"
```

| 线程 | 1 GB / 1900 万行，-n 8，无压缩 | zstd |
|------|------|------|
| 1 | 2.8 s | 3.5 s |
| 2 | 1.2 s | — |
| **4** | **1.0 s** | **1.5 s** |
| 8 | 1.1 s | — |

**难点不是并行，是「从哪开始读」。** 按 `file_len/n` 直接 seek 会落在一条记录中间，
甚至落在带引号换行的字段内部；而判断"这里能否安全开始"需要引号状态，
那只能从字节 0 扫过来才知道。

所以计数那一遍会顺带记录**检查点**：每 1000 条记录的字节偏移，且保证当时扫描器处于
引号外状态。线程从检查点起读（最多跳过 999 条），并且每个线程负责完整的若干分片——
记录不会跨线程切分、重复或乱序。

`tests/roundtrip.rs` 里有一条测试专门比对这个不变量：同一文件用 1/2/4/8 线程切，
合并回来的字节必须完全相同。

## 实战记录 / Field run

一份 1.0 GB / 1900 万行的真实文件（6 列，含引号内换行的备注字段），整条链路跑一遍，
每步都开 `--verify`。Apple M1 Pro，release build（数字来自脚本自动采集，可复现）：

| 步骤 | 耗时 | 结果 |
|------|------|------|
| `preview` 首次打开（建索引） | **0.01 s**（索引已有，148 KB） | ✓ |
| `preview --start-row 18000000` | **0.56 s** | ✓ 跳到第 1800 万行 |
| `analyze` 全列画像 | 10.9 s | ✓ |
| `search` 全列找词 | 8.1 s | 扫完 1900 万行 |
| `filter -o --verify` | 5.6 s | ✓ 校验 2,370,440 行 × 6 列 |
| `dedupe --by cust_no --verify` | 5.2 s | ✓ kept 63,191 + removed 18,936,809 = 19,000,000 |
| `groupby` 三聚合 | 6.3 s | ✓ 1900 万行 → 4 组 |
| `sort` 双列（city 升序 / amount 降序） | 25.8 s | ✓ 外部归并 |
| `columns --verify` | 11.9 s | ✓ 校验 1900 万行 × 5 列 |
| `partition --by city` | 19.8 s | ✓ 4 个分区，zst 压缩 |
| `split -n 8 --threads 4` | **1.1 s** | ✓ 多线程读 |
| `merge` 回单文件 | **0.69 s** | ✓ 1900万条，1008.9 MB |
| `verify` 无损自检 | 5.2 s | ✓ 1057918702 字节逐字节一致 |

内存峰值（`/usr/bin/time -l`，maximum resident set）：

| 操作 | 峰值 |
|---|---|
| `analyze` | **29 MB** |
| `split --threads 4` | 121 MB |
| `sort`（外部归并，`--buf-mb 64`） | 227 MB |

**结论**：1900 万行的每一步都在 30 秒内完成，没有一步需要把文件装进内存。
`analyze` 和 `search` 是最慢的两个（都要读全表一次），这是因为它们做的事就是"看完整个文件"。

## 性能 / Performance

一台普通笔记本上的实测（`cargo build --release` 同理，debug 构建更快但内存相近）：

| 场景 | 数据量 | 结果 |
|------|--------|------|
| 建立行索引 | 87 MB / 160 万行 | **0.53 s**，峰值内存 **11.8 MB**，索引 **13 KB** |
| 建立行索引 | 1.0 GB / 1900 万行 | **0.54 s**，峰值内存 **10.9 MB**，索引 **148 KB** |
| **建立行索引** | **9.9 GB / 1.9 亿行** | **4.28 s**，峰值内存 **18.3 MB**，索引 **1.45 MB** |
| 跳到第 18,000,000 行 | 1.0 GB | **0.49 ms**，峰值内存 **3.5 MB** |
| **跳到第 189,000,000 行** | **9.9 GB** | **0.5–1.2 ms**，峰值内存 **11.3 MB** |
| 拆成 8 片 | 1.0 GB | **2.8 s**（≈370 MB/s），峰值内存 **88 MB** |
| **拆成 8 片，4 线程** | 1.0 GB | **1.0 s**（≈1 GB/s） |
| **拆成 8 片** | **9.9 GB** | **20.9 s**（≈490 MB/s），峰值内存 **89 MB** |
| 合并回单文件 | 1.0 GB | **1.05 s**，峰值内存 **4.8 MB** |
| **合并回单文件** | **9.9 GB** | **6.1 s**，峰值内存 **10.7 MB** |
| **SQL 聚合（5 组）** | **9.9 GB / 1.9 亿行** | **2.78 s**（DuckDB，多核） |
| **SQL 分页（第 100 万行起）** | **9.9 GB** | **4.25 s**（含全表排序），首屏 2.60 s |
| 无损校验（split → merge → 字节比较） | 1.0 GB / 9.9 GB | 逐字节一致 |

> 关键点：**索引体积是文件大小的 1/7000**（9.9 GB → 1.45 MB），因为每 1000 行只记一个字节偏移；
> 跳转成本与文件大小无关。内存不随文件增长：拆分峰值 89 MB、合并 10.7 MB，与文件大小无关。
> 按同样比例，100 亿行的索引约 76 MB —— 而把这份数据塞进表格软件需要几百 GB。

## Supported Formats / 支持的压缩格式

| Format | Compress | Decompress |
|--------|:--------:|:----------:|
| Zstandard (.zst) | ✅ | ✅ |
| Gzip (.gz) | ✅ | ✅ |
| Bzip2 (.bz2) | ✅ | ✅ |
| XZ/LZMA (.xz) | ✅ | ✅ |
| Zip (.zip) | ✅ | ✅ |
| 7-Zip (.7z) | — | ✅ |
| Tar (.tar) | ✅ | ✅ |
| Tar+Gzip (.tar.gz) | — | ✅ |
| Tar+Zstd (.tar.zst) | — | ✅ |
| Tar+Bzip2 (.tar.bz2) | — | ✅ |
| Tar+XZ (.tar.xz) | — | ✅ |

## Build from Source / 从源码构建

Requires **Rust 1.75+**.

```bash
# Clone
git clone https://github.com/Li-Binzhuang/csv-splitter.git
cd csv-splitter

# Dev build
cargo build

# Release build (with LTO)
cargo build --release

# Run the GUI
cargo run --release -p csv-splitter-gui

# Run the CLI
cargo run --release -p csv-splitter-cli -- --help
```

### Platform Dependencies / 平台依赖

| Platform | Dependencies |
|----------|-------------|
| macOS | None |
| Windows | None |
| Linux | CJK font + GTK3 dev libs |

Linux setup:

```bash
# Ubuntu / Debian
sudo apt install fonts-noto-cjk libgtk-3-dev libxdo-dev

# Arch
sudo pacman -S noto-fonts-cjk gtk3

# Fedora
sudo dnf install google-noto-sans-cjk-fonts gtk3-devel
```

## Project Structure / 项目结构

```
csv-splitter/
├── Cargo.toml                 # Workspace root
├── crates/
│   ├── core/                  # Core library (no GUI dependency)
│   │   └── src/
│   │       ├── types.rs       # SplitConfig, ProgressEvent, etc.
│   │       ├── csv_reader.rs  # CSV parsing, header reading, delimiter detection
│   │       ├── splitter.rs    # Split + column filter + compress pipeline
│   │       ├── compress.rs    # Compressor factory (Zstd/Gzip/Bzip2/XZ)
│   │       ├── decompress.rs  # Decompression (12 format combos)
│   │       ├── preview.rs     # Data preview + column stats (reservoir sampling)
│   │       └── sort.rs        # External merge sort
│   ├── cli/                   # Headless CLI (split / merge / verify / stats)
│   │   └── src/main.rs
│   └── gui/                   # GUI application (egui / eframe)
│       └── src/
│           ├── main.rs        # Entry point
│           ├── app.rs         # App framework + drag-drop handling
│           ├── state.rs       # Per-tab state definitions
│           ├── theme.rs       # Dark theme + CJK font loading
│           ├── i18n.rs        # Chinese / English i18n
│           ├── widgets/       # Shared UI components
│           └── tabs/          # Feature pages (split, preview, stats, decompress)
```

## CI/CD

| Trigger | Workflow | Description |
|---------|----------|-------------|
| Push to `main` / PR | CI | Check, Clippy (-D warnings), Tests |
| Tag `v*` push | Release | Build all platforms → GitHub Release |
| Manual dispatch | Release | Optional tag input, build with/without release |

CI 为什么拆成两个 job、怎么让它不变慢，见 [docs/CI.md](docs/CI.md)。

To create a release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Release artifacts are automatically uploaded with SHA256 checksums. Tags containing `-alpha`, `-beta`, `-rc` are marked as pre-release.

## License

[MIT](LICENSE)