# CI notes / 持续集成说明

两个 job，按「反馈速度」而不是「模块」划分：

| job | 内容 | 依赖 |
|-----|------|------|
| `engine` | fmt、core+gui 的测试、性能护栏、clippy | 无（**不含 DuckDB**） |
| `cli` | clippy、CLI 与 HTTP API 测试 | DuckDB（静态编译，冷缓存约 10 分钟） |

## 为什么 DuckDB 要单独一个 job

`duckdb` crate 用 `bundled` 特性，CI 上要从 C++ 源码编译一遍。把它和引擎测试放在同一个
job，意味着改一行 `cache.rs` 也要等十分钟才知道结果。拆开后，引擎的问题几分钟内就有答案。

## 三条让 CI 不变慢的规则

1. **不要在提交里随手改版本号。**
   `Cargo.lock` 的哈希是 cargo 缓存键的一部分；改一次版本号就等于换一次键，
   DuckDB 得从头再编。版本号按里程碑批量升（0.18.0 → 0.19.0 一次），
   中间的功能提交保持 lock 不动。

2. **压缩产物**：`CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0`。
   DuckDB 的 rlib 带调试信息是 **1.6 GB**，去掉后 **346 MB**。缓存能存下、传输也快。

3. **不要为「证明能编译」再开一个 profile**。
   基准测试用 criterion，另建 bench profile 只为编译检查，代价约 2.5 分钟——
   本地能跑的东西不必占用 CI。

## 本地怎么复现 CI

```bash
cargo fmt --all -- --check
cargo test -p csv-splitter-core -p csv-splitter-gui
cargo test -p csv-splitter-core --test performance
cargo clippy -p csv-splitter-core -p csv-splitter-gui --all-targets -- -D warnings

CARGO_PROFILE_TEST_DEBUG=0 cargo clippy -p csv-splitter-cli --all-targets -- -D warnings
CARGO_PROFILE_TEST_DEBUG=0 cargo test -p csv-splitter-cli
```
