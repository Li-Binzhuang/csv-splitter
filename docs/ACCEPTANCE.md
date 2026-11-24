# 验收对照 / Acceptance

目标是「一个完整的 CLI + 后端」，前端由别人接。下面每一项都指向**能被执行的东西**，
不是描述。

## 1. 砍掉 GUI

| 证据 | 位置 |
|---|---|
| 工作区只有两个 crate | `Cargo.toml`：`members = ["crates/core", "crates/cli"]` |
| GUI 目录与依赖已删 | `git log --diff-filter=D -- crates/gui`；`eframe`/`egui`/`rfd` 不在依赖里 |
| CI 与发布构建 CLI | `.github/workflows/ci.yml`、`release.yml` 构建 `csvx` |

验证：`cargo metadata --no-deps | grep csvx-` 输出只有 core 和 cli。

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



## 平台构建矩阵（本地已验证 vs CI 负责）

| 平台 | 构建方式 | 证据 |
|---|---|---|
| macOS arm64 | 本机 | `release-check.sh` 全绿，产物 64.1 MB / `tar.gz` 19.5 MB |
| macOS x86_64 | 本机交叉编译 | `cargo build --release --target x86_64-apple-darwin` 成功（2m37s），产物 65.5 MB；**同一套 release-check 通过**（Rosetta 运行）|
| Linux x86_64 | CI（`ubuntu-22.04`）| 每次 push 的 `CI` job 编译并测试 |
| Windows x86_64 | **CI（windows-latest，MSVC）** | `Windows check` run 3（`1095f33`）**通过**：`cargo check -p csvx-cli` 在真实 MSVC 工具链上编译含 DuckDB 的全部依赖。本机另用 mingw GCC 14 交叉编译出 PE32+ 可执行文件（91.7 MB）作为补充证据 |

## 发布与部署（goal：能发布 / 能部署 / 能跨平台）

| 项 | 证据 |
|---|---|
| 三平台构建 | `release.yml` 构建 linux-x86_64 / macos-arm64 / macos-x86_64 / windows-x86_64，产物 + `.sha256` |
| Windows 编译 | Unix-only 代码（信号、symlink）已 `cfg(unix)` 隔离；本机用 mingw-w64 **GCC 14** 交叉编译通过（含 DuckDB）。CI 通道：`windows.yml` 在 push（`crates/**` 变更）用 windows-latest 的 MSVC 跑 `cargo check`。ubuntu+mingw 被否决：GCC 10 的 win32 线程模型缺 `std::mutex`，DuckDB 编译失败（有 annotation 为证）|
| CI 无 GUI 依赖 | 两个工作流都不再安装 libgtk-3-dev / libxdo-dev / fonts-noto-cjk |
| 缓存不撑爆配额 | 两个工作流都用滚动 key（`...-rolling-<run_id>`）|
| 部署材料 | [`DEPLOY.md`](DEPLOY.md) + [`deploy/csvx.service`](../deploy/csvx.service) |
| 产物可运行 | `scripts/release-check.sh`：解压 → **校验和比对** → `demo` → `serve --check` → `/health` → `/spec` → `/rows` → SIGTERM，全过；`tar.gz` 19.5 MB |
| 三条安装路径 | `scripts/install-check.sh`：源码构建 / `cargo install --path` / release 产物 各跑 `--version` + `demo` + `serve --check`，三条全绿 |
| 发布分支逻辑 | `scripts/release-decision.sh --self-test`：tag push / dispatch+tag / dispatch 空跑 / 非 tag 拒绝 / 版本不一致拒绝，7 个用例；CI 与 regress 都跑 |
| 部署前自检 | `serve --check`（根目录可读、端口可绑），systemd 用它做 `ExecStartPre`；`backend-smoke.sh` 也在起服务前跑一次 |
| 文档输出逐字一致 | `serve_check_output_matches_deploy_doc`：DEPLOY 里引用的 `serve --check` 输出必须与真实输出同形（版本号除外）|
| 排障表 | DEPLOY「出问题的时候」：7 个症状 → 原因 → 处理，含 `ready:false`、路径越界、冷档 1.4 s、504 |
| 配置项 | `--root` / `--port` / `--addr`、`CSVX_QUERY_TIMEOUT_MS`，`/capabilities` 发布生效值 |
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
cargo build --release -p csvx-cli
bash scripts/dev.sh gate            # 全量测试（含 core 的 sql 特性）
bash scripts/smoke.sh               # CLI 主路径
bash scripts/regress.sh             # 每条命令一次
bash scripts/backend-smoke.sh       # 起真服务，44 项断言

# 数字（2026-11-10，本版）
#   workspace 测试 494（core 带 sql 特性时为 275 个库测试）
#   冒烟 43 步 / 全命令扫 39 项 / 后端 44 项
```

四项脚本全绿，就是这份对照表的证据。


## 能力面验收（3.48.0）

| 新能力 | CLI | HTTP | 测试 |
|---|---|---|---|
| 拼接多文件 | `concat a.csv b.csv` | `POST /concat`（job）| `concat_stacks_files_over_http` + smoke |
| 列变换写新文件 | `columns --keep/--drop/--rename` | `POST /columns`（job）| `columns_reshape_writes_a_new_file` |
| 列集合对比 | `schema --diff b.csv` | `GET /schema?diff=` | `schema_diff_matches_the_cli`（断言字节相同）|
| 追加公式列 | `edit formula --name --value` | `action=formula` | `edit_formula_appends_a_computed_column` |
| **重算已有列** | `edit set-formula --col --value` | `action=set-formula` | 3 个 core + 1 个 HTTP（含循环引用拒绝）|
| 一列拆多列 | `edit split-column` | `action=split-column` | `edit_split_column_is_reachable_over_http` |
| 按 rowKey 插行 | `edit insert --after-key b:12` | `action=insert-after-key` | `insert_by_key_anchors_to_the_named_row` |
| 多列区间填充（一次撤销）| `edit fill --cols a,b` | `action=fill&cols=` | 2 个 core + 1 个 HTTP + smoke |
| 数据校验 | `validate --check` | `GET /validate[&limit]` | `validate_reports_rules_over_http`、`validate_can_bound_its_scan` |
| 跨表查找 | `lookup --key --from --get` | `POST /lookup`（job）| `lookup_copies_columns_by_key` |
| 编码转换 | `convert --to utf-8` | `POST /convert`（job）| `convert_rewrites_the_encoding` |

契约检查（`crates/cli/tests/contract.rs`）：

| 断言 | 说明 |
|---|---|
| `/capabilities` 的 `editActions` = dispatch | 双向；抓出过 `delete-where` 未声明 |
| `/spec` 的路由 = 服务的路由 | 双向；抓出过虚构的 `/eval` |


## Operations 契约验收（2026-11-24）

目标：把 csvx 演进为本地 AI 原生 CSV 工作台。以下数字来自本机 release 构建或发布包实测。

### 一键复现

```bash
bash scripts/gate.sh                 # 全量测试 + clippy + docs
bash scripts/demo_end_to_end.sh      # CLI + MCP SDK 端到端
python3 scripts/smoke_http.py        # 发布包 HTTP 面
bash scripts/package_release.sh      # 打包并 verify_dist
```

### 里程碑

| 里程碑 | 证据 | 结果 |
|---|---|---|
| Stage 2 共享操作目录 | `csvx ops --json` / `GET /ops` / MCP `csvx://operations` | 17 操作，三端同一份 |
| Stage 2.5 typed operations | `SchemaRequest` / `FilterRequest` / `ViewRequest`；HTTP `/schema` `/filter` `/rows` | schema/filter typed CLI+HTTP；view core + HTTP 简单无编辑窗口；复杂 where/order 仍 SQL |
| Stage 3 preview/apply | `csvx_edit_preview` / `preview_edit` / `EditApplyRequest` | CLI/HTTP/MCP preview；CLI/HTTP 共用 typed apply，MCP 走同一 CLI 路径 |
| 压缩输出 | `filter -o out.csv.gz` | 56 MB → 7.3 MB，0.40 s |
| Pipeline 质量门禁 | `gate` 单文件/目录，CLI/HTTP/MCP | 失败 JSON 带 `failures`；HTTP 200 + `passed:false`；MCP `isError` |
| 多文件/目录 | `concat` CLI/HTTP/MCP；`gate` 目录 | HTTP 目录排序并排除输出；4M 行 concat 0.59 s（MCP） |
| 跨文件唯一性 | `gate --unique-global` / `uniqueGlobal` | 4M 行、2 文件：7.03 s，短失败原因 |
| 报表 spec | `report` CLI/HTTP/MCP | summary/columns/quality/groups/chart SVG/compare/rows；内联 `specText` |
| 报表 count chart | `chart,city` | 2M 行：Markdown 0.44 s / 147 B；HTML SVG 0.48 s / 872 B |
| 报表安全 | HTTP/MCP `--root` | compare 第二文件路径不能越 root |
| 发布包 HTTP | `smoke_http.py` | 7/7 检查通过（含 opt-in viewer 可达），errors=[] |

### 约束核对

- 本地单用户：无登录、无多用户鉴权、无遥测。
- 无 GUI：未新增桌面/图形界面；报表是可打开的 Markdown/HTML 静态文件。
- xlsx：未启用。
- 网络：核心命令与 MCP 不依赖网络。
- 计费：未接入 license/计费。

### 尚未收尾

- **薄前端**：与“无 GUI”硬约束存在范围冲突；现有 opt-in 网页 viewer 可达（smoke 检查 `GET /` 200），但未作为 GUI 产品推广；是否算薄前端需用户裁决（见 `docs/NIGHT_QUEUE.md`）。
- **xlsx / 数据库只读连接**：需用户明确解除约束后才能做。

### 当前产物

- MCP 工具：23。
- AOCI：123 entries, aligned。
- 包：`dist/csvx-4.2.0-darwin-arm64.tar.gz`，以最新构建 SHA 为准。
