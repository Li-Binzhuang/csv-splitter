# CI notes / 持续集成说明

**现在是一个 job。** 这是被一次事故逼回来的：

| 时间 | 结构 | 现象 |
|---|---|---|
| 早期 | `engine` + `cli`（`needs` 串联） | 正常，engine 先绿 |
| 2026-11-10 | 同上 | CI 红，但 **cli job 被跳过、0 个步骤** |
| 随后 | 同上 | 红，且 **jobs 列表为空** |
| 现在 | 单个 `test` job | 完整信号 |

根因是**缓存配额**：当时仓库里有两条 4.1 GB 的 `cli` target 缓存，加上 engine 的两条约
9.5 GB，超过 GitHub 每仓库 10 GB 的上限。之后运行开始"红而没有可读原因"——
一个红叉，点进去什么都没有。

`needs` 把两个 job 绑在一起，于是"少了一半"看起来像整体失败；而**单 job 不会丢失一半的自己**。

代价是引擎改动也要编一次 DuckDB。用滚动缓存后是几分钟，换一个永远完整的绿叉，值。

## 三条让 CI 不变慢的规则

1. **缓存键用滚动式，不要用 lockfile 哈希。**
   `key: linux-cargo-rolling-${{ github.run_id }}` + `restore-keys: linux-cargo-rolling-`
   = 每个 job 只保留**一条**缓存。用 lockfile 哈希时每条提交都会存一份新的 4 GB target，
   两条就撑爆配额，然后 CI 开始红得莫名其妙。
   （版本号仍然按里程碑批量升，减少无谓的完整重编。）

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

## 实测

两次连续提交的 CI 墙钟时间：

| 改动 | engine | cli | 总墙钟 |
|------|--------|-----|--------|
| 拆分 job 后首次（含冷编译） | 427 s | ~1750 s | ≈29 min |
| 去掉 debug info + 去重后（冷缓存） | **220 s** | **783 s** | **1009 s（17 min）** |
| 同上，紧接着的下一次提交（**命中缓存**） | — | — | **226 s（3.8 min）** |

engine 减半是因为它本来就不碰 DuckDB，只是时间被 fmt/clippy 的缓存状态影响；
cli 减半主要来自 `liblibduckdb_sys.rlib` 从 1.6 GB 降到 **346 MB**——缓存省的不是编译时间，
而是每个 job 结束时那段「打包/上传 target」。

第三行是关键验证：**只要 `Cargo.lock` 不动，下一次推送就是 3.8 分钟**。
这也解释了为什么第一条规则（别随手升版本号）比任何构建技巧都重要——
以前每轮都升版本，等于每轮都吃一次 17–29 分钟的冷启动。


## 反馈回路 / Feedback loop

一晚上做不出多少功能，通常不是代码慢，而是**每个改动都跑了一遍全量验证**。

| 命令 | 耗时 | 什么时候用 |
|---|---|---|
| `scripts/dev.sh check` | **~3 秒** | 改代码的过程中，每次改完 |
| `scripts/dev.sh test -p core --lib mod` | **~5 秒** | 一个功能写完了，只跑相关单元测试 |
| `scripts/dev.sh gate` | **~90 秒** | 攒了几个提交，推送前跑一次 |
| CI | **~10 分钟** | 推送后，**不要等**，下一轮再回来看 |

规矩：

1. 编辑时只用 `check`（类型检查，不链接、不生成测试二进制）。
2. 功能写完跑一次**相关**测试，不要 `--workspace`。
3. 推送前跑一次 `gate`。
4. 一轮最多推一次，推完立刻继续干活，CI 结果下一轮再看。

## 本地磁盘 / Local disk

压测文件和构建产物是这台机器上最大的两块占用，前者曾经累积到 8 GB，后者到 72 GB。

```bash
scripts/clean.sh          # 清掉压测残留（几秒）
scripts/clean.sh --all    # 连 target/ 一起清（下次构建要重来，约 2 分钟）
```

`target/` 之所以能到几十 GB：DuckDB 是 bundled 编译的静态库，每个 profile
各存一份，再加上默认的 DWARF 调试信息。`Cargo.toml` 里把 `[profile.dev]` 和
`[profile.test]` 的 `debug` 关掉了，同样的测试全量运行后 `target/` 是 **3 GB**
而不是 72 GB。

写压测脚本时用 `TempDir`（测试里已经在用），手工造的大文件用完立刻 `rm -rf`。

