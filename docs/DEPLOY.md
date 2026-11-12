# 部署 / Deploying

这个后端是**单机单用户**的：没有鉴权、默认只监听 loopback、一个进程服务一个目录。
把它当成本机或内网里的一件工具，而不是多租户服务。

## 三种安装方式

| 方式 | 命令 | 适用 |
|---|---|---|
| 发布产物 | 解压 `csv-splitter-<平台>.tar.gz`，里面就是单个二进制 | 不想装 Rust |
| `cargo install` | `cargo install --path crates/cli` | 本机已有 Rust |
| 源码构建 | `cargo build --release -p csv-splitter-cli` | 要改代码 |

发布产物按平台命名：`csv-splitter-linux-x86_64`、`csv-splitter-macos-arm64`、
`csv-splitter-macos-x86_64`、`csv-splitter-windows-x86_64`（`.zip`），每个都带 `.sha256`。

解压后的最小验证：

```bash
./csv-splitter --version
./csv-splitter demo -o /tmp/x && ./csv-splitter preview /tmp/x/orders.csv --rows 3
```


## 验证发布产物（不进网络也能验）

仓库里有两个脚本，让「用户拿到的东西」在本地就能验一遍：

```bash
scripts/package.sh --build csv-splitter-linux-x86_64   # 打包 + sha256，产物在 dist/
scripts/release-check.sh                                # 打包后解压，跑三条最小验证
```

`release-check.sh` 做的事和用户完全一样：解压 → `--version` → `demo` → `serve` →
`/health`、`/spec`、`/rows` → `SIGTERM` 停机。发布流程调用的是**同一个**
`scripts/package.sh`，所以本地验过的打包步骤就是 tag 时跑的步骤。

本机实测（3.28.0，macOS arm64）：

| 项 | 值 |
|---|---|
| 二进制 | 64.1 MB（DuckDB 静态链接）|
| `tar.gz` | 19.5 MB |
| 校验和 | `dist/*.tar.gz.sha256` |

## 起服务

```bash
csv-splitter serve --root /srv/csv-splitter --port 8787
# "csv-splitter API on http://127.0.0.1:8787  (root: /srv/csv-splitter)"
```

| 参数 | 默认 | 说明 |
|---|---|---|
| `--root` | 当前目录 | 只服务这个目录下的文件；越界与符号链接一律拒 |
| `--port` | 8787 | 绑到 `127.0.0.1:<port>` |
| `--addr` | — | 完整绑定地址；`127.0.0.1:0` 让内核挑端口 |
| `--check` | — | **只做自检然后退出**：根目录可读、端口可绑、版本打印；失败退出码 1 |

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `CSV_SPLITTER_QUERY_TIMEOUT_MS` | 30000 | 单条 SQL / 过滤查询的时间预算；`/capabilities` 会发布生效值 |

部署前先自检（systemd 的 `ExecStartPre` 就是这么用的）：

```bash
$ csv-splitter serve --check --root /srv/csv-splitter --port 8787
  ok     version                3.29.0 (linux)
  ok     root                   /srv/csv-splitter
  ok     root readable          the directory lists
  ok     bind                   127.0.0.1:8787
== configuration ok ==
$ echo $?
0
```

根目录不存在或端口被占时退出码是 1，并明确指出哪一项失败。

启动后的三个探针：

```bash
curl -s 127.0.0.1:8787/health   # {"ok":true,"ready":true,...}  ← ready 才接流量
curl -s 127.0.0.1:8787/spec     # 契约
curl -s 127.0.0.1:8787/metrics  # p50/p95、错误分布、任务
```

## systemd

仓库里有 [`deploy/csv-splitter.service`](../deploy/csv-splitter.service)：

```bash
sudo cp deploy/csv-splitter.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now csv-splitter
journalctl -u csv-splitter -f
```

- `SIGTERM` 是优雅停机：不再接新连接、在途请求收尾、在跑的 job 最多再等 5 秒，退出码 0
- `Ready` 的判断看 `/health` 的 `ready`，不是 `ok`

## 磁盘上会出现什么

| 文件 | 谁写的 | 删掉的后果 |
|---|---|---|
| `<file>.csv`（你的数据） | **没人写**，只读 | — |
| `<file>.journal` | 编辑操作（append-only） | **删掉会丢编辑**，别删 |
| `<file>.idx` | 行索引，自动生成 | 删除只损失一次重建（2.5 GB 约 0.3 s） |
| `<file>.parquet` | 列式缓存（`POST /parquet` 或 `cache --build`） | 删除只损失构建时间（2.5 GB 约 11 s / 427 MB） |

升级与回滚：**换二进制即可**。数据在 CSV 与 `.journal` 里，sidecar 可随时重建；
回滚就是换回旧二进制，不需要迁移。

## 观测

- `/metrics` 的 `routes.*.maxMs` 找慢端点，`slowRequests` 是 ≥1 s 的计数
- 每个响应带 `X-Response-Time`，前端也能读
- `jobs.running` 是「现在能不能安全停机」的依据

## 安全清单

| 项 | 状态 |
|---|---|
| 默认只监听 loopback | ✅ 绑非 loopback 时启动会打 warning |
| 路径越界 / 符号链接 | ✅ 400，有测试 |
| 请求上限（查询串 16 KB / 行 24 KB / 头 64 行） | ✅ |
| 慢客户端 | ✅ 读 10 s / 写 30 s 超时 |
| 鉴权 | ❌ **没有**。要跨机器访问，请放在带鉴权的反向代理后面 |
| 多用户隔离 | ❌ **没有**。一个进程一个目录，编辑是单用户语义 |

## 平台差异

| 平台 | 差异 |
|---|---|
| Linux / macOS | `SIGTERM`/`SIGINT` 走优雅停机 |
| Windows | 没有可捕获的 SIGTERM；`Ctrl+C` 直接结束进程。CLI 与后端功能一致 |
