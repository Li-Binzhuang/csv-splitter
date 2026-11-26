# csvx 性能基准（可复现）

所有数字来自 `--release` 二进制，同一台机器：**Apple M5 / 16 GiB RAM / macOS 27.2.1 arm64**。
用 `/usr/bin/time -l` 记录 wall time 与 peak RSS。没有做系统级 drop cache，因此“冷/热”只表示本次进程的前后两次，不代表磁盘缓存被清空。数字会随机器与缓存变化；以当前代码重跑为准。

## 0. 可复现 harness（新增）

本机环境：`Apple M5`、`16 GiB` 内存、`macOS 27.2.1`、`arm64`。其他机器必须重跑，
不得直接引用本机数字作为 SLA。

`scripts/bench_100gb.py` 流式生成确定性 CSV，然后依次跑：

```text
schema --json        (schema/header scan)
filter --where city=CDMX
search ZZZUNIQUE
analyze --quality --json
```

每条命令记录 wall time 与 peak RSS，输出 `<out>.bench.json`、`<out>.bench.md`
和脱敏的 `<out>.summary.json`（不含原始路径/行值）。smoke 会对同一行数生成两次并
逐字节比较，验证数据确定性。

100 GB 实际运行（2026-11-26，release 二进制，生成确定性数据）：

```text
bytes=99,999,999,969 rows=1,259,779,856
generate 128,676.9 ms
schema  70,186.3 ms / 34.67 MB
filter  23,547.5 ms / 99.63 MB
search  18,720.1 ms / 97.08 MB
quality 456,003.1 ms / 17.96 MB
total   568,457.0 ms
```

这是本机实际生成的 100 GB 文件；未清 page cache，quality 占约 80% 总时间。
C 主线旧的 104.5 GB 数字保留在 §1，不和本表混用。机器可读摘要见
[`bench-100gb-2026-11-26.json`](bench-100gb-2026-11-26.json)。

1 GB 实际运行（2026-11-26，release 二进制，生成确定性数据）：

```text
bytes=999,999,971 rows=12,597,801
schema 636.7 ms / 27.66 MB
filter 178.5 ms / 79.69 MB
search 108.2 ms / 77.20 MB
quality 4804.3 ms / 18.42 MB
total 5727.7 ms
```

这是 1 GB 实测，不是 100 GB 承诺。

小规模 smoke（本轮实测，1000 行 / 77,403 bytes，非 100GB 承诺）：

```text
schema 361.6 ms / 69.34 MB
filter 330.8 ms / 51.48 MB
search 323.9 ms / 51.58 MB
quality 332.0 ms / 53.13 MB
total 1348.3 ms
```

复现完整 100 GB：

```bash
python3 scripts/bench_100gb.py --gb 100 --out bench/orders.csv --binary target/release/csvx
```

该命令实际生成/扫描 100 GB；上面是本机实测。其他机器需重跑后替换数字，smoke
数字不能当作 100 GB 性能。

## 1. 100 GB 级（C 主线，2026-11-23）

**文件**：104,492,590,800 bytes（约 97.4 GiB / 104.5 GB），1,300,001,300 行，4 列
`id,city,amount,note`；由 100 MB 块重复生成，用于扫描路径而非真实分布。

| 操作 | 命令 | 耗时 | 峰值 RSS | 结果 |
|---|---|---:|---:|---|
| 首屏 | `preview big.csv` | 0.02 s | 8 MB | 首屏 |
| 建稀疏索引 | `index big.csv` | 24.57 s | 21 MB | `.idx` 9.9 MB |
| search（第 1 次） | `search big.csv ZZZUNIQUE --limit 10` | 20.18 s | 93 MB | 0 命中，全文件扫描 |
| search（第 2 次） | 同上 | 20.42 s | 93 MB | 0 命中 |
| filter（无输出） | `filter big.csv --where city=CDMX` | 25.75 s | 95 MB | 1,300,001,299 scanned / 260,000,000 matched |
| filter -o（选择性） | `filter big.csv --where id=1 -o sel.csv` | 19.54 s | 17 MB | 输出 94 KB（1,300 行） |
| **filter -o（全量 20%）** | `filter big.csv --where city=CDMX -o cdmx.csv` | **79.62 s** | **28 MB** | **200,000,000 行 / 20 GB** |
| 深跳 | `view big.csv --start 700000000 --count 5` | 0.02 s | 29 MB | 第 7 亿行 |

**C 验收**：100 GB search <60 s、filter（扫描/无输出）<90 s、RSS <200 MB —— **满足**。
**全量物化**（200,000,000 行、20 GB 输出）用两遍扫描 + 预计算偏移：第一遍只数每个
slab 的输出字节，第二遍直接写最终文件的不相交区间；**79.62 s / 28 MB**，首次进入 90 s。
但两遍只在大文件回本：同一 29 GB 文件 A/B，选择性 `filter -o` **part 7.21 s vs 两遍
14.52 s**，20% 输出 **15.18 vs 17.90 s**。因此按输入大小自适应（阈值 64 GB）：
- ≥64 GB：两遍扫描；100 GB 省掉 20 GB part 写入，79.62 s。
- <64 GB：保留 part 并行 copy；29 GB 选择性 7.64 s、20% 输出 15.16 s，与旧路径一致；
  2.3 GB 1.57 s。

复现：

```bash
# 100 MB 块，重复 1300 次
awk 'BEGIN{print "id,city,amount,note"; for(i=1;i<=1000000;i++) printf "%d,%s,%d,abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ\n", i, (i%5==0)?"CDMX":"Puebla", i%1000 }' > block.csv
for i in $(seq 1 1300); do cat block.csv >> big.csv; done
csvx preview big.csv
csvx index big.csv
/usr/bin/time -l csvx search big.csv ZZZUNIQUE --limit 10
/usr/bin/time -l csvx filter big.csv --where city=CDMX
```

## 1b. 81 GB / 11.6 亿行 7 列（更接近真实字段）

文件 87,389,224,844 bytes，7 列（id/city/region/amount/customer/note/ts），城市比例
17/23/31/29%；仍由 21.8 GB 块重复 4 次（不是独立真实数据）。

| 操作 | 耗时 | RSS | 结果 |
|---|---:|---:|---|
| search（0 命中，全扫） | 39.42 s | 87 MB | — |
| filter city=CDMX（无输出） | 46.70 s | 86 MB | — |
| filter -o（全量 17%） | **62.03 s** | **28 MB** | 196,928,881 行 / 13 GB |

按字节线性估算 100 GB 约 48/57/76 s，但**这是估算，不是实测**；100 GB 的实测仍以
104.5 GB 合成文件为准。

| 2.3 GB .csv.zst（51 MB） | filter 4.16 s/15 MB；search 12.13 s/11 MB；quality 21.73 s/20 MB | 输出与 plain `cmp` 一致；顺序读 |
| 2.3 GB zst search materialize | 11.87 s/12 MB → 3.51 s/89 MB | 命中 1900 万一致；解压到临时文件后随机/并行读 |
| 2.3 GB zst filter materialize | 扫描 6.94→3.62 s（12→91 MB）；导出 4.06→2.60 s（16→63 MB） | 1900 万命中，322 MB 输出 `cmp` 一致 |
### release 冒烟（2026-11-23，4.2.0，2.3 GB / 9500 万行）

| 操作 | 墙钟 | 进程 peak RSS |
|---|---:|---:|
| filter city=CDMX（扫描） | 2.78 s | 88 MB |
| filter -o（1900 万行 / 411 MB） | 0.71 s | 60 MB |
| search CDMX | 1.57 s | 86 MB |
| analyze --quality | 20.79 s | 18.3 MB |

单次 release 二进制运行，未清 page cache；同一文件此前 pilot 报告出现 0.69/0.43 s
等更快数字，属于缓存与运行方差，不构成 SLA。

### MCP 解压缓存命中（2026-11-23，2.04 GB / 1.2 亿行）

| 调用 | 墙钟 | 扫描行数 | 进程 peak RSS |
|---|---:|---:|---:|
| `csvx_search` gzip + materialize 第一次 | 4.61 s | 120,000,000 | 86 MB（MCP server） |
| 同一文件第二次（缓存命中） | 1.63 s | 120,000,000 | 同上 |

- 文件：2.04 GB CSV，gzip 后 426 MB；查询 `ZZZ` 无命中，强制扫全量。
- 首次含解压 2.04 GB 到临时 CSV；第二次复用缓存，不再解压。
- 重复查询约 **2.8×**；gzip 压缩耗时（4.30 s）不计入。
- 未清 page cache，非 SLA；缓存上限默认 4 GiB，本测试文件 2.04 GB 可容纳。

复现：`python3 scripts/bench_mcp_cache.py --binary <csvx> --root <dir> --file <compressed.csv.gz>`。

### MCP 缓存：zstd / bzip2（2026-11-23，110 MB / 700 万行）

| 输入 | 第一次（含解压） | 第二次（缓存命中） | 扫描行数 |
|---|---:|---:|---:|
| `orders.csv.zst`（3.2 MB） | 0.279 s | 0.101 s | 7,000,000 |
| `orders.csv.bz2`（7.4 MB） | 0.693 s | 0.100 s | 7,000,000 |

- MCP server peak RSS 85 MB；查询 `ZZZ` 无命中，强制扫全量。2 GB+ 复测（120M 行）：2026-11-23 4.545→1.590 s；11-24 第一轮 4.914→1.726 s；
11-24 第二轮 4.573→1.594 s；三次 RSS 均 82 MB，第一轮按噪声处理。
- 首次含解压到临时 CSV；第二次复用缓存。
- 未清 page cache；非 SLA；110 MB 文件远小于默认 4 GiB 上限。

### 压缩解码多线程评估（2026-11-23）

- 1 GB CSV / 6000 万行，zstd 压缩后 24 MB；
- `zstd -d -T1`：0.94 s / 0.93 s；`-T4`：0.92 s / 0.92 s；
- 单帧 zstd 解码不会因 `-T` 明显并行化，收益约 2%。
- 结论：暂不引入外部 `zstd` CLI 依赖；csvx 保持内置单线程解码 + `--materialize` 路径。

## 2. 其他已验证数字（摘要）

| 场景 | 文件/规模 | 结果 |
|---|---|---|
| filter -o 无命中 | 11 GB / 2.8 亿行 | 2.61 s，RSS 17–77 MB |
| filter -o 5600 万命中 | 11 GB → 2.2 GB 输出 | 5.26 s |
| search | 31 GB / 7.8 亿行 | 7.38 s，RSS 81 MB |
| filter | 31 GB | 10.13 s，RSS 81 MB |
| edit compact | 1000 万 journal | 8.98 s，压后打开/导出 0.02 s |
| pipeline 全量 | 2.3 GB / 9500 万行，filter→lookup→groupby | 6.55 s（0.65/0.70/0.40 s） |
| pipeline 增量 | 同上，未变 | 17 ms |
| pipeline DAG 扇出 | 2.3 GB / 9500 万行：filter → filter + groupby 并发 | 7.60 s；增量 0.02 s；父进程 peak 187 MB（含并发子进程） |
| quality | 2.03 GB / 5600 万行 | 18.71 s，RSS 17 MB |
| convert | 2.2 GB → Parquet | 5.36 s；回程 2.15 s，逐字节一致 |
| MCP 深分页 | 2.3 GB / 9500 万行 | 0.02 s（warm）；cold schema 3.11 s |
| MCP quality | 2.3 GB | 22.25 s |

## 3. 已知边界

- 100 GB 文件是重复块，分布不代表真实数据；搜索/过滤按字节/行扫描，结果仍可比较。
- 没有清空 page cache；同一文件多次运行会受缓存影响。
- 内存是进程 peak RSS，不含 OS 缓存；多进程 pipeline 的各步单独计。
- 未测 100 GB 的并发读写、全量 filter -o、压缩文件直接扫描；这些不能由本表推断。
