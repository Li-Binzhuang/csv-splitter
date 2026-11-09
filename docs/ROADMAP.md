# Roadmap / 路线图

> 目标：任意大小的 CSV 都能「秒开、滚动、切分」，内存不随文件增长。
> 对标 WPS 表格的查看体验，但不把文件读进内存。

## 北极星指标 / North star

| 指标 | 目标 | 现状 |
|---|---|---|
| 打开 10 GB 文件到可交互 | ≤ 5 s | **4.28 s**（9.9 GB / 1.9 亿行实测） |
| 跳转任意行 | ≤ 10 ms | **211 µs**（160 万行）/ **0.56 s 首跳**（1 GB，含建索引） |
| 常驻内存（查看） | ≤ 200 MB 且与文件大小无关 | **11.3 MB**（10 GB 跳转）/ 29 MB（1 GB 全列 analyze） |
| 切分吞吐 | ≥ 80 MB/s | **490 MB/s**（9.9 GB 实测） |
| 无损 | 100% | `verify` 字节级通过 |

## 里程碑 / Milestones

### M1 · 随机访问地基 — `done`
- [x] 引号感知的记录读取（多行字段、CRLF、BOM、无尾换行）
- [x] 稀疏行索引（每 1000 行一个字节偏移，`.idx` 边车文件）
- [x] `index` 命令 + `preview --start-row`
- [x] 索引格式版本化、原子写入、文件变更自动重建

### M2 · 查看体验（GUI 冻结，转为 API） — `done`
- [x] HTTP API：`/health` `/meta` `/rows` `/stats`，根目录白名单 + CORS + 回环绑定
- [x] `/rows?start=N` 窗口化读取，20 页/秒不重扫
- [x] `preview --start-row` 跳转任意行（索引失效自动重建）
- [x] **内置浏览页面** `GET /`（单文件 HTML，只用公开端点）
- [x] 页面契约测试：端点、字段、默认端口
- [x] 大文件打开时的进度与 ETA（`spawn_reporter`）
- [-] ~~GUI 预览 / 列宽 / 冻结列~~：GUI 冻结，不再投入
- [x] 1 GB / 1900 万行实测
- [x] **9.9 GB / 1.9 亿行实测并写进 README**

### M3 · 编码与格式 — `done`
- [x] 编码探测：UTF-8 / UTF-8 BOM / GBK / UTF-16 LE/BE（BOM 与 NUL 模式）
- [x] 预览 / 统计 / 切分 / API 全部按识别结果解码
- [x] UTF-16 下的行索引明确拒绝（字节偏移会错位），UTF-8 与 GBK 可随机访问
- [x] 转码命令 `convert`（GBK / UTF-16 → UTF-8，流式、恒定内存）
- [x] Windows-1252（西欧 ANSI）与 GBK 打分选择，`café` 不再乱码
- [x] 零字节 / 只有表头 / 纯空白行 / 无尾换行 / 字段含 NUL / 2 MB 单行 —— 全部实测
- [x] 分隔符显式指定：`-d comma|tab|semicolon|pipe|<char>`
- [ ] 引号 / 转义风格的显式覆盖（目前按 RFC 4180）

### M4 · 分析能力 — `done`
- [ ] 列统计改为索引均匀采样（百万行毫秒级）— 未做；全表 analyze 1 GB 约 11 s
- [x] 按列筛选（=、~、>、<、empty）+ 返回行号，可配合 `preview --start-row` 跳转
- [x] `/filter` 只读 API
- [x] SQL 分析（DuckDB，只读、单语句、结果封顶）→ `sql` 命令 + `/query` API
- [x] `/rows?sql=` 分页：查询结果按窗口返回，`has_more` 无需 COUNT
- [x] 全局搜索（跨所有列，返回行号）→ `search` + `GET /search`
- [x] `analyze` 全列画像（空值/去重/数值统计/ragged 计数）
- [x] `groupby` 流式聚合（count/sum/mean/min/max，内存随分组数）
- [x] `dedupe` / `sort` / `columns` / `partition` / `concat` 等交付前整理
- [x] 每个写操作都能 `--verify` 自证

### M5 · 规模化 — `done`
- [x] 生成 9.9 GB 合成数据做压测与内存基线（见 README「实战记录」）
- [x] 基准测试（criterion）+ 回归护栏 `tests/performance.rs`
- [x] 写操作任务化：`POST /split` `POST /merge` + job id 轮询
- [x] 查询结果缓存：重复翻页 0 ms，前缀语义明确
- [x] `/warm` 后台预热：用户开始滚动时缓存已就绪
- [x] 多线程切分（检查点起点，1 GB 从 2.8 s → 1.0 s）
- [x] CI 单 job（两-job 结构在缓存配额事故后合并回一个）
- [ ] Windows 路径容错（只在 macOS / Linux 实测过）
- [ ] 大文件并发读（当前每个任务单读多写）

## 现在能做什么 / What it does today

一条从「拿到一份陌生 CSV」到「交付一份干净文件」的链路，每一步都有 CLI 和 HTTP 两种入口：

```
files → preview → analyze → search/filter → dedupe → sort → columns
      → groupby → partition/concat → split/merge → verify → export/download
```

自证能力：`verify`（拆分逐字节）+ `--verify`（filter/columns/dedupe/sort/partition 计数）。

## 已知短板 / Known gaps

| 短板 | 影响 |
|---|---|
| 没在真浏览器里点过内置页面 | 有契约测试兜底，但布局/样式未经人眼验证 |
| `analyze` 是全表扫描，1 GB 约 11 s | 数据量大时不够快（索引采样方案未做） |
| 引号/转义风格不可覆盖 | 非 RFC 4180 方言的文件可能解析错 |
| Windows 未实测 | 路径处理可能有坑 |
| `docs/ROADMAP.md` 之外的中文文档 | 与代码同步靠测试，未全覆盖 |

## 非目标 / Non-goals

- 不做公式、图表、透视表这类电子表格能力（不是 WPS 的替代品）。
- 不修改原文件；所有操作产出新文件。
- 不引入数据库或服务端；单机二进制。

## 迭代纪律 / Iteration rules

1. 一个迭代 = 一个可以独立解释的提交；测试、文档、CHANGELOG 同批提交。
2. 先有可测的证据，再有结论：每个性能数字都必须能在 README 里复现。
3. 失败要响：宁可报错，不产出少一条记录的文件（`verify` 是最后的兜底）。
4. 推送攒批，CI 必须绿。
