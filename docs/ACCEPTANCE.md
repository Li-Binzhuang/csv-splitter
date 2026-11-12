# 验收对照 / Acceptance

目标是「一个完整的 CLI + 后端」，前端由别人接。下面每一项都指向**能被执行的东西**，
不是描述。

## 1. 砍掉 GUI

| 证据 | 位置 |
|---|---|
| 工作区只有两个 crate | `Cargo.toml`：`members = ["crates/core", "crates/cli"]` |
| GUI 目录与依赖已删 | `git log --diff-filter=D -- crates/gui`；`eframe`/`egui`/`rfd` 不在依赖里 |
| CI 与发布构建 CLI | `.github/workflows/ci.yml`、`release.yml` 构建 `csv-splitter` |

验证：`cargo metadata --no-deps | grep csv-splitter-` 输出只有 core 和 cli。

## 2. 列元数据与存在性

| 层 | 证据 |
|---|---|
| 库 | `sheet::schema(sample)`、`sheet::column_index(name)`（`crates/core/src/sheet.rs`）|
| CLI | `schema FILE`、`schema --has COL`（存在 0 / 不存在 1）、`--json`、`--sample`、`--diff`、`--require` |
| HTTP | `GET /columns`、`/columns?name=`、`?kind=`、`?limit=`；`/schema` 为别名 |
| 测试 | `schema_lists_columns_and_answers_existence`、`schema_require_asserts_a_column_set`、`schema_diff_reports_column_set_changes`、`columns_filter_and_limit`、`columns_and_compare_and_set` |
| 782 列不被截断 | `backend-smoke.sh` 断言 `"count":6`；真实 2.5 GB 文件实测返回 **782 条、107 KB、0.67 s** |

## 3. 行列交前端

| 层 | 证据 |
|---|---|
| 库 | `Pager::page`（窗口）、`Window.keys`（行身份）|
| CLI | `view --columns`、`preview --columns` |
| HTTP | `GET /rows`：`start/count/requested/limit/total/pastEnd/totalExact/columns/frozenColumns/rows/rowKeys`；`columns=`、`start-col=`、`count-cols=`、`keys=`、`order=`、`where=` |
| 测试 | `the_row_cap_is_stated_in_the_response`、`window_modifiers_compose`、`keys_compose_with_projection_and_paging`、`cell_keys_and_horizontal_paging`、`an_empty_page_says_why_it_is_empty`、`a_filtered_page_says_when_its_total_is_exact` |
| 深偏移常数时间 | 真实 2.5 GB 文件：第 680 000 行 **2.4 ms**（全列 88 KB）、3 列 **0.9 ms**（667 字节）|

## 4. 写入约束

| 层 | 证据 |
|---|---|
| CLI | `edit set --expect VALUE`（不符则非 0 退出并报实际值）|
| HTTP | `POST /edit?action=set&expect=` 不符 **409**，带 `actual`/`expected` |
| 测试 | `expect_turns_a_set_into_compare_and_set`、`columns_and_compare_and_set`、`every_edit_action_is_advertised_in_the_hint` |
| 脚本 | `backend-smoke.sh` 的 `expect` 三态（写成功 / 409 / 值变了再 409）|
| 值约束校验 | `validate --check "col=value"`（等值规则可用；真实文件 `predict_status=success` 0 违规）|

## 5. 三条链闭合 + 原文件不可变 + 大文件实测

| 事实 | 证据 |
|---|---|
| 路由 ↔ 能力 ↔ 文档 | `READ_ROUTES`/`WRITE_ROUTES` 单一来源；`every_served_route_is_advertised`、`the_endpoint_index_covers_the_capabilities` |
| CLI 帮助 ↔ 解析的参数 | `every_command_documents_the_flags_it_parses` |
| 文档引文 ↔ 二进制实际输出 | `troubleshooting_quotes_are_real` |
| 原文件不可变 | `no_command_modifies_the_input_file`；2.5 GB 文件 sha256 在全部操作后不变 |
| 大文件实测 | `docs/CAPABILITIES.md` 的实测表：index 0.31 s / 15.9 MB / 5.4 KB；导出 2.5 GB 从 40.7 s → **2.2 s**（字节相同）|
| 文档 → 代码（反向） | `every_documented_flag_exists`、`every_documented_route_is_served`：文档里提到的每个 flag/端点都必须真实存在（这两条抓到过 `serve --port` 不存在）|
| 状态码契约 | `failure_kinds_have_one_status_code_each`：17 种失败各自的码 |
| 一套可跑的门 | `scripts/dev.sh gate`（**494 测试**）、`smoke.sh`（43）、`regress.sh`（39）、`backend-smoke.sh`（**44**）|



## 性能两档（2.5 GB / 782 列 / 684 917 行，本机实测）

| 调用 | 冷档（无缓存无索引） | 暖档（`POST /parquet` 后） | 契约 |
|---|---|---|---|
| `/rows` 不过滤 | 1–2 ms | 1–2 ms | ≤ 10 ms |
| `/rows?where=` 首屏（命中密集）| **0.30 s** | **0.26 s** | 冷 ≤1 s / 暖 ≤0.35 s |
| `/rows?where=` 0 命中（全扫）| 1.36 s | ~0.2 s | 冷档是 IO 下限，文档写明 |
| `/rows?order=` 首屏 | 1.36 s | 0.34 s | 暖 ≤0.35 s |
| `/columns` 全量 782 列 | **8 ms** | 8 ms | ≤0.5 s |
| `/columns?name=` | 7 ms | 7 ms | ≤0.5 s |
| `/meta` | 0.24 s | 0.24 s | ≤0.5 s |
| `/cell` | 1.6 ms | 1.6 ms | — |

**`/sheet` 会先告诉客户端自己在哪一档**：

```json
"cost":{"tier":"cold","filterFirstPageMs":1000,"orderFirstPageMs":1400,
        "hint":"POST /parquet … moves this sheet to the fast tier"}
```

- `/columns` 不再为了行数建索引：没有索引时 `rows: null`，**明说不知道**，而不是花 0.3 秒去算
- `/meta` 是唯一会（且只一次）建索引的端点，0.24 s 在预算内


## 发布与部署（goal：能发布 / 能部署 / 能跨平台）

| 项 | 证据 |
|---|---|
| 三平台构建 | `release.yml` 构建 linux-x86_64 / macos-arm64 / macos-x86_64 / windows-x86_64，产物 + `.sha256` |
| Windows 编译 | Unix-only 代码（信号、symlink）已 `cfg(unix)` 隔离；`windows-check` job 在 PR 与手动触发时跑 `cargo check -p csv-splitter-cli`（windows-latest）|
| CI 无 GUI 依赖 | 两个工作流都不再安装 libgtk-3-dev / libxdo-dev / fonts-noto-cjk |
| 缓存不撑爆配额 | 两个工作流都用滚动 key（`...-rolling-<run_id>`）|
| 部署材料 | [`DEPLOY.md`](DEPLOY.md) + [`deploy/csv-splitter.service`](../deploy/csv-splitter.service) |
| 产物可运行 | `scripts/release-check.sh`：解压 → `--version` 3.28.0 → `demo` → `/health` → `/spec` → `/rows` → SIGTERM，全过；`tar.gz` 19.5 MB |
| 配置项 | `--root` / `--port` / `--addr`、`CSV_SPLITTER_QUERY_TIMEOUT_MS`，`/capabilities` 发布生效值 |
| 升级/回滚 | 换二进制即可；`.idx`/`.parquet` 可重建，只有 `.journal` 是数据 |
| 边界写明 | 单机单用户、无鉴权、默认 loopback |

## 交付验收（3.25.0，真实 2.5 GB / 782 列 / 684 917 行，全新副本）

**原文件 sha256**：`34e8d6f9…578d3` 前后一致；服务运行后目录里只有 `real.csv`、`.idx`、`.parquet` 三个文件，原文件未被写入。

### 浏览器与契约

| 检查 | 结果 |
|---|---|
| `OPTIONS` 预检 | **204**，`Allow-Methods: GET, POST, HEAD, OPTIONS` |
| `HEAD /rows` | **200**，`Content-Length` 与 GET 一致，**无 body** |
| `ETag` + `If-None-Match` | **304**，0.002 s，不重传 JSON |
| `Accept-Encoding: gzip` | 500 行窗口 **22290 → 5714 字节**（3.9×），ETag 带 `-gz` |
| `GET /spec` | 13.6 ms / 2191 字节 |
| `GET /capabilities` | 2.5 ms / 1085 字节 |

### 冷档（无缓存；`.idx` 由 `/meta` 建一次）

| 调用 | 时间 | 契约 | 达标 |
|---|---|---|---|
| `/health` | 20 ms（首个请求）| — | ✓ |
| `/columns?name=cust_no` | **56 ms** | ≤0.5 s | ✓ |
| `/columns` 全量 782 列 | **10 ms** / 107 KB | ≤0.5 s | ✓ |
| `/meta` | **374 ms**（建索引一次）| ≤0.5 s | ✓ |
| `/sheet`（cost 字段）| 6.5 ms | — | ✓ |
| `/rows` 不过滤 | 7 ms | ≤10 ms | ✓ |
| `/rows?where` 首屏（全命中）| **302 ms** | ≤1 s | ✓ |
| `/cell` | 3 ms | — | ✓ |
| `/journal` | 0.34 ms | — | ✓ |
| `/metrics` | 0.5 ms | — | ✓ |

### 暖档（`POST /parquet`，构建 12 s / 427 MB）

| 调用 | 首次（页缓存冷）| 稳态 | 契约 | 达标 |
|---|---|---|---|---|
| `/rows?where` 首屏 | 0.40 s | **0.23–0.26 s** | ≤0.35 s | ✓ |
| `/rows?order` 首屏 | 0.36 s | 0.30–0.37 s | ≈0.35 s（偶发 0.37）| ⚠ 记录在案 |
| `/rows?where` 0 命中 | 0.10 s | 0.10 s | — | ✓ |
| `/sheet` 的 `cost.tier` | `cold` → `warm` | — | 会翻转 | ✓ |

### 运维与稳健性

| 检查 | 结果 |
|---|---|
| `SIGTERM` | 进程自行退出，端口随后拒绝连接 |
| `job` 取消 | `404` 未知 / `200` 已完成 / `409` 不可取消 / `202` 受理 |
| 客户端中途断开 ×5 | 服务照常 `/health` 200 |
| 20 并发请求 | 全部 200，`/metrics` 计数 |
| 查询超时 | `504`，**不含** `INTERRUPT`/`Binder Error`/`detail` |

## 首屏延迟（2.5 GB / 782 列，冷启动、无缓存、无索引）

| 场景 | 之前 | 现在 |
|---|---|---|
| `/rows?where=predict_status=success&count=10` | 8–9 s | **0.30 s**（第二次 0.17 s）|
| 另一列全命中过滤 | 8–9 s | **0.16 s** |
| 命中在中段 / 0 命中（必须读数据）| 15 s | 1.36–1.48 s |
| 排序首屏 | 15.5 s | 1.36 s（有列式缓存 0.34 s）|
| `count(*)` | 8.6 s | 0.71 s |

三个原因，分别修掉：类型推断整表扫描、方言探测整表扫描、64 行采样却建了行索引。
**目录副作用**：一次冷启动请求后目录里仍然只有 `real.csv`（不产生 `.idx`、不写原文件）。
**契约不变**：`count/requested/limit` 三字段、`total` + `totalExact`、过滤页无 `rowKeys`、
30 秒超时中断，全部有测试钉住。

## 框架自身的契约

| 契约 | 钉住它的东西 |
|---|---|
| 状态码一类一码 | `failure_kinds_have_one_status_code_each`（17 例）|
| 窗口上限 2000 三字段一致 | `the_row_cap_is_stated_in_the_response` + `backend-smoke.sh` |
| 过滤页 `total` 是下界时明说 | `a_filtered_page_says_when_its_total_is_exact` |
| 空页面区分「空文件 / 翻过头」 | `an_empty_page_says_why_it_is_empty` |
| 过滤页不给会指错行的 `rowKeys` | `a_filtered_page_says_when_its_total_is_exact` |
| 变更探测器（`stamp`）会动 | `the_sheet_carries_a_stamp_that_moves_after_a_write` |
| 查询超时会真的中断 | `a_zero_timeout_interrupts_the_engine_and_says_so` |
| 文档不撒谎 | `every_documented_flag_exists`、`every_documented_route_is_served` |

## 怎么复现

```bash
cargo build --release -p csv-splitter-cli
bash scripts/dev.sh gate            # 全量测试（含 core 的 sql 特性）
bash scripts/smoke.sh               # CLI 主路径
bash scripts/regress.sh             # 每条命令一次
bash scripts/backend-smoke.sh       # 起真服务，44 项断言

# 数字（2026-11-10，本版）
#   workspace 测试 494（core 带 sql 特性时为 275 个库测试）
#   冒烟 43 步 / 全命令扫 39 项 / 后端 44 项
```

四项脚本全绿，就是这份对照表的证据。
