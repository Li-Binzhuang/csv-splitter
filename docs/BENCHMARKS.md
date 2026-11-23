# csvx 性能基准（可复现）

所有数字来自 `--release` 二进制，同一台 macOS / Apple Silicon 机器；用 `/usr/bin/time -l` 记录 wall time 与 peak RSS。没有做系统级 drop cache，因此“冷/热”只表示本次进程的前后两次，不代表磁盘缓存被清空。数字会随机器与缓存变化；以当前代码重跑为准。

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
| **filter -o（全量 20%）** | `filter big.csv --where city=CDMX -o cdmx.csv` | **101.93 s** | **25 MB** | **200,000,000 行 / 20 GB** |
| 深跳 | `view big.csv --start 700000000 --count 5` | 0.02 s | 29 MB | 第 7 亿行 |

**C 验收**：100 GB search <60 s、filter（扫描/无输出）<90 s、RSS <200 MB —— **满足**。
但**全量物化**（200,000,000 行、20 GB 输出）实测 **101.93 s**，超过 90 s：瓶颈在
并行 part 文件再串行拼接到最终文件（约 40 GB 写入）；这是已识别缺口，下一步优化目标是
避免双写，而不是把它算作扫描达标。

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
| quality | 2.03 GB / 5600 万行 | 18.71 s，RSS 17 MB |
| convert | 2.2 GB → Parquet | 5.36 s；回程 2.15 s，逐字节一致 |
| MCP 深分页 | 2.3 GB / 9500 万行 | 0.02 s（warm）；cold schema 3.11 s |
| MCP quality | 2.3 GB | 22.25 s |

## 3. 已知边界

- 100 GB 文件是重复块，分布不代表真实数据；搜索/过滤按字节/行扫描，结果仍可比较。
- 没有清空 page cache；同一文件多次运行会受缓存影响。
- 内存是进程 peak RSS，不含 OS 缓存；多进程 pipeline 的各步单独计。
- 未测 100 GB 的并发读写、全量 filter -o、压缩文件直接扫描；这些不能由本表推断。
