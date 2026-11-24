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
bash scripts/package_release.sh   # dist tar.gz + .sha256，末尾自动 verify_dist（失败会删半成品）
bash scripts/verify_dist.sh dist/csvx-4.2.0-<platform>.tar.gz   # 可选手工复验
```

## 3. 2026-11-23 本机验证
- release 构建：30.28 s；
- demo：pipeline 15/13 ms；Python 与官方 SDK 均 `gzip_matched=3`、`status_fresh=2`；SDK `pipeline_progress_events=2`、`progress_events=1`、`structured_ok=true`、`job=done`；
- MCP 缓存 drill：2 GB+ gzip 重查 4.545→1.590 s，server peak 82 MB；
- macOS gate：workflow_dispatch run `45198239384` 12m21s；push run `45202447374` 11m32s；均 success；
- 包：`csvx-4.2.0-darwin-arm64.tar.gz` 约 20 MB；已含 17 工具（含 cache_stats）、pipeline watch（含 `--json`、失败退避、`--max-failures`、`--state`（含成功/失败时间戳/轮次恢复））、job 保留/退出取消、缓存 LRU/hits-misses 统计、`bench_mcp_cache.py`、`PILOT_INTAKE.md`、`ROADMAP.md`；
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
