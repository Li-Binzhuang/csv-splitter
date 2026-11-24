# csvx 路线图复核（2026-11-23）

> 依据本仓库实测与已交付资产；未测数字不写。商业化决策需用户确认。

## 当前可交付能力

| 主线 | 已验证能力 | 证据 |
|---|---|---|
| A 工作台 | 声明式 Pipeline/DAG、增量快跳过、内容哈希、原子提交、血缘、崩溃恢复、进度标记 | `docs/DEMO.md`；pipeline 测试；2.3 GB DAG 7.60 s、增量 0.02 s |
| B MCP | 16 工具、严格 schema、游标、编辑锁、job、structuredContent、progress、压缩 materialize | 官方 SDK 1.32.2；`pipeline_progress_events=2`、`status_fresh=2`、`gzip_matched=3` |
| C 引擎 | 100 GB search/filter、81 GB 7 列、压缩输入、并行 filter | 100 GB search 20.18 s/93 MB、filter 25.75 s/95 MB、export 79.62 s/28 MB |
| 商业化 | BUSINESS/BENCHMARKS/SALES/OUTREACH/PILOT/RELEASE_CHECKLIST、离线包、demo | `dist/` 产物与 `docs/BUSINESS.md` |

## 硬件与边界

- 已验证平台：macOS arm64；其他平台需在目标机重建并重跑 demo。
- 硬约束：不做 GUI、不做多用户鉴权、不打 tag、xlsx 未启用。
- 无 License 强制、无遥测。

## 仍存缺口（按优先级）

1. **真实客户试点**：目前只有合成样本与 demo，没有真实客户数据/反馈/付费承诺。商业化主线卡在这里，需要用户提供样本或联系人。
2. **压缩多线程**：已实测 zstd 单帧 `-T4` 相对 `-T1` 仅约 2%，不引入外部 CLI；保持内置单线程解码，`--materialize` 仍是主要加速路径。
3. **MCP 解压缓存**：已实现并实测 gzip 2.04 GB 4.61→1.63 s、zst 0.279→0.101 s、bz2 0.693→0.100 s；后续只需监控命中率与容量。
4. **默认内容哈希阈值**：1 MiB 硬编码，仅能用 `CSVX_CONTENT_CHECK_LIMIT` 调整；大文件仍可能被同尺寸+mtime 改写欺骗。
5. **发布自动化**：包与 checklist 仍是手动脚本；已有 Ubuntu/Windows CI，macOS gate 已手动跑绿（run 45198239384，12m21s）。

## 下一阶段（30/60/90 天口径）

- **30 天**：按 `docs/PILOT_INTAKE.md` 拿到 1 个真实试点样本，跑通 pipeline + MCP + 报告；记录真实耗时/RSS；决定是否收费。
- **60 天**：按试点反馈补 1 个最高价值缺口（优先压缩多线程）；产出第二份 benchmark。
- **90 天**：形成可签约的报价/交付边界；若仍无真实客户，则把产品定位收敛为“顾问式本地数据工作台”，不虚构 SaaS。

## 决策规则

- 先量后改：任何优化先有可复现测量，再动代码。
- 每个里程碑最多一次 push；gate 绿、AOCI aligned、tree clean。
- 商业化价格/合同/SLA 由用户确认，不自行承诺。

## 产品进化计划（2026-11-24 起）

产品形态与阶段验收见 [`PRODUCT_PLAN.md`](PRODUCT_PLAN.md)：
S0 契约目录、S1 三端目录已完成；S2 typed operations、S3 preview/apply、
S4 压缩输出/质量门禁/多文件、S5 薄前端、S6 报表、S7 数据库/xlsx 按顺序推进。

## Round 190 checkpoint（2026-11-24）

自 Round 175 后新增/关闭：
- watch 状态与 `--json` 都带 `nextDelayMs`，重启在未达上限时沿用；
- 失败 JSON 实测 `success=false`/`nextDelayMs=20`/原始错误；
- 退避封顶实测停在 20，重启 20→40；
- 离线包重建并自验（18 工具，SHA `6d3c9f04`）。

技术侧仍无新的高价值缺口；真实客户试点由用户决定。

## Round 175 checkpoint（2026-11-24）

自 Round 159 后新增/关闭：
- 18 个 MCP 工具全部带 annotations（readOnly/openWorld/destructive）；
- Python 与官方 SDK demo 都验证 `annotated_tools=18` / `sdk_annotated=18`；
- `cache_clear` 清扫死进程临时文件并返回 `staleRemoved`；
- 最新 2 GB+ drill 4.573→1.594 s / 82 MB；
- 离线包重建并自验（18 工具，SHA `2e8d39d7`）。

技术侧仍无新的高价值缺口；真实客户试点由用户决定。

## Round 159 checkpoint（2026-11-24）

自 Round 150 后新增/关闭：
- 第 17/18 个 MCP 工具 `csvx_cache_clear` / `csvx_cache_stats`；
- 两个缓存工具都不可提交为 job（`server tool` 错误）；
- Python 与官方 SDK demo 命中缓存、清空并复查 `cache_after=0`；
- 离线包重建并自验（18 工具，SHA `45ab459b`）。

技术侧没有新的高价值缺口；真实客户试点仍由用户决定。

## Round 150 checkpoint（2026-11-24）

自 Round 137 后新增/关闭：
- watch `--state` 生命周期完整：轮次续接、失败计数持久、`--max-failures` 跨重启生效、
  `lastError` 落盘与重启时打印上次原因；
- MCP 18 工具（cache_stats/clear）已在 Python/官方 SDK demo 中命中并清空；
- job_submit 通用 CLI 白名单；
- 最新 2 GB+ gzip drill 4.914→1.726 s / 82 MB，与 4.545→1.590 同量级；
- 离线包重建并自验（SHA `c0f3c55f`）。

技术侧仍无新的高价值缺口；真实客户试点由用户决定。

## Round 137 checkpoint（2026-11-24）

自 Round 113 后新增/关闭：
- `pipeline watch` 完整化：立即跑、轮询、`--json`、失败 2× 退避、`--max-failures`、
  `--state`（时间戳/轮次恢复）；
- 第 17/18 个工具 `csvx_cache_clear` / `csvx_cache_stats`（清临时文件、命中/容量）；
- `csvx_job_submit` 改为 CLI 工具白名单，server 内部/未知工具直接 `job_start_error`；
- 解压缓存 LRU 命中重排；
- 2 GB+ gzip drill 4.545→1.590 s / 82 MB；
- 离线包重建并自验（18 工具含 cache_clear、watch、job 白名单）。

技术侧没有新的高价值缺口；真实客户试点仍由用户决定，不阻塞继续维护。

## Round 113 checkpoint（2026-11-23）

自 Round 100 后新增/关闭：
- MCP job 历史上限 128、退出取消、两条单测；
- 解压缓存 LRU + 可选 hits/misses 统计；
- 2 GB+ gzip drill 4.545→1.590 s / 82 MB；
- 压缩多线程实测 `-T4` 仅约 2%，明确不引入外部 CLI；
- macOS CI dispatch 与 push 两条路径均绿（12m21s / 11m32s）；
- 离线包重建并自验（LRU、job 保留、cache stats 已包含）。

唯一外部缺口仍是真实客户试点；技术侧没有已知高价值缺口。

## Round 100 checkpoint（2026-11-23）

已稳定：
- A：Pipeline/DAG、内容哈希（≤1 MiB 默认 + `--content-check`）、实时 `CSVX_STEP`、原子提交、血缘/恢复；
- B：16 MCP 工具、progress、job、严格 schema、解压缓存与启动清理；
- C：100 GB search 20.18 s / filter 25.75 s；压缩 materialize + gzip/zst/bz2 缓存复测；
- CI：Ubuntu/Windows 已有；macOS gate 手动与 push 两条路径均绿（12m21s / 11m32s）；
- 交付：离线包、`verify_dist` 自验、demo、BENCHMARKS/ROADMAP/PILOT_INTAKE/RELEASE_CHECKLIST。

唯一外部缺口：
- 真实客户试点：需要用户提供脱敏样本或联系人；没有真实数据就无法给出付费结论。

剩余技术项（不阻塞试点）：
- 压缩解压仍单线程；job 历史无上限；缓存命中率未持续监控。

决策：目标保持 active；不打 tag、不加 License 强制/遥测；价格/SLA 需用户确认。
