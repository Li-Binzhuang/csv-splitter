# 发布检查清单（4.2.0）

> 本地离线分发用；不打 tag、不发布 registry、不含遥测。

## 1. 发布前
- [ ] `cargo test --workspace --all-targets` / `scripts/gate.sh` 全绿；
- [ ] `cargo build --release` 成功；
- [ ] `scripts/demo_end_to_end.sh` 通过（会拒绝旧于源码的 release binary）；
- [ ] 夹具与 `dist/` 临时文件已清理。

## 2. 构建与打包
```bash
cargo build --release
bash scripts/demo_end_to_end.sh
python3 scripts/smoke_http.py   # 发布包 HTTP：/ops、/report chart+compare、/gate global、/concat dir
bash scripts/package_release.sh   # dist tar.gz + .sha256，末尾自动 verify_dist（失败会删半成品）
bash scripts/verify_dist.sh dist/csvx-4.2.0-<platform>.tar.gz   # 可选手工复验
bash scripts/verify_dist_negative.sh                            # 坏包必须被拒绝
# 包内 Agent：解包后启动 agent serve，跑 12 路由 + 428/deny/approve + 多动作 replay + SSE
# 验收日志末尾含 stats: ... 与 stats_json: {...}；verify_dist 断言 routes=12、replay=actions、sse_ms<250
# 构建日志：dist/csvx-4.2.0-<platform>.verify.log（UTC 时间 + tarball SHA256 + 全部验收输出）
```

## 3. 2026-11-23 本机验证
- release 构建：30.28 s；
- demo：pipeline 15/13 ms；Python 与官方 SDK 均 `gzip_matched=3`、`status_fresh=2`；SDK `pipeline_progress_events=2`、`progress_events=1`、`structured_ok=true`、`job=done`；
- MCP 缓存 drill：2 GB+ gzip 重查 4.545→1.590 s，server peak 82 MB；
- macOS gate：workflow_dispatch run `45198239384` 12m21s；push run `45202447374` 11m32s；均 success；
- 敏感信息扫描：token/key/私钥模式与敏感路径均 0 命中；
- 包冒烟：解包二进制通过 `--version`、`pipeline --help`、watch state（nextDelayMs=5000；失败 lastError/nextDelay 20→40 重启/封顶 20/失败 JSON 20/failures=1 实测/上限拒绝）、tools/list 18/18 annotations；
- 包：`csvx-4.2.0-darwin-arm64.tar.gz` 约 20 MB；已含 23 工具（23 工具 annotations（demo 验证 23/23）；cache_stats/clear + stale sweep，Python/SDK demo 命中/清空/死进程/复查 0），均不可 job；白名单）、pipeline watch（demo 覆盖，含 `--json`、失败退避、`--max-failures`、`--state`（nextDelay（重启沿用）/成功/失败时间戳/轮次恢复/lastError/失败上限/上次原因））、job 保留/退出取消、缓存 LRU/hits-misses 统计、`bench_mcp_cache.py`、`PILOT_INTAKE.md`、`ROADMAP.md`；
- SHA256：构建时由 `package_release.sh` 输出，不在此处固定（会随构建变化）。

> 本清单随离线包一起分发（`docs/RELEASE_CHECKLIST.md`）。

## 4. 交付
- 同时交付 tar.gz 与 `.sha256`；接收方先 `shasum -a 256 -c ...`；
- 发布方可用 `scripts/verify_dist.sh` 校验 checksum、必需成员与包内二进制版本；
- 首次运行建议 `csvx --version` 核对 `4.2.0` 与 DuckDB 编译状态；
- macOS arm64 已验证；其他平台需在目标平台重建并重跑 demo。

## 5. 边界
- xlsx 仍未启用（硬约束）；
- 无 GUI、无多用户鉴权、无 License 强制；
- demo 的熟练计时非 SLA，性能承诺只写 `docs/BENCHMARKS.md` 实测数字。
