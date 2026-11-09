# CSV Splitter Pro

> Cross-platform CSV splitter, compressor, previewer & column statistics tool with bilingual (Chinese/English) UI.
>
> 跨平台 CSV 文件拆分、压缩、预览与列统计工具，支持中英文界面。

[![CI](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/ci.yml/badge.svg)](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/ci.yml)
[![Release](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/release.yml/badge.svg)](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/release.yml)
[![GitHub Release](https://img.shields.io/github/v/release/Li-Binzhuang/csv-splitter?include_prereleases&sort=semver)](https://github.com/Li-Binzhuang/csv-splitter/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

---

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
| 🌐 HTTP API | Read-only JSON API for a web front-end, pages huge files by row | 只读 JSON 接口，供前端分页浏览超大文件 |

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

## 性能 / Performance

一台普通笔记本上的实测（`cargo build --release` 同理，debug 构建更快但内存相近）：

| 场景 | 数据量 | 结果 |
|------|--------|------|
| 建立行索引 | 87 MB / 160 万行 | **0.53 s**，峰值内存 **11.8 MB**，索引 **13 KB** |
| 建立行索引 | 1.0 GB / 1900 万行 | **0.54 s**，峰值内存 **10.9 MB**，索引 **148 KB** |
| **建立行索引** | **9.9 GB / 1.9 亿行** | **4.28 s**，峰值内存 **18.3 MB**，索引 **1.45 MB** |
| 跳到第 18,000,000 行 | 1.0 GB | **0.49 ms**，峰值内存 **3.5 MB** |
| **跳到第 189,000,000 行** | **9.9 GB** | **0.5–1.2 ms**，峰值内存 **11.3 MB** |
| 拆成 8 片 | 1.0 GB | **2.42 s**（≈420 MB/s），峰值内存 **88 MB** |
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