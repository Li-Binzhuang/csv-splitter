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
| 782 列不被截断 | `backend-smoke.sh` 断言 `"count":6`；真实 2.5 GB 文件实测返回 782 条、107 KB |

## 3. 行列交前端

| 层 | 证据 |
|---|---|
| 库 | `Pager::page`（窗口）、`Window.keys`（行身份）|
| CLI | `view --columns`、`preview --columns` |
| HTTP | `GET /rows`：`start/count/requested/limit/total/pastEnd/totalExact/columns/frozenColumns/rows/rowKeys`；`columns=`、`start-col=`、`count-cols=`、`keys=`、`order=`、`where=` |
| 测试 | `the_row_cap_is_stated_in_the_response`、`window_modifiers_compose`、`keys_compose_with_projection_and_paging`、`cell_keys_and_horizontal_paging`、`an_empty_page_says_why_it_is_empty`、`a_filtered_page_says_when_its_total_is_exact` |
| 深偏移常数时间 | 真实 2.5 GB 文件：第 680 000 行 **2.0 ms**（全列 87 KB）、3 列 **0.9 ms**（651 字节）|

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
| 一套可跑的门 | `scripts/dev.sh gate`、`smoke.sh`（43）、`regress.sh`（40）、`backend-smoke.sh`（44）|

## 怎么复现

```bash
cargo build --release -p csv-splitter-cli
bash scripts/dev.sh gate            # 全量测试（含 core 的 sql 特性）
bash scripts/smoke.sh               # CLI 主路径
bash scripts/regress.sh             # 每条命令一次
bash scripts/backend-smoke.sh       # 起真服务，44 项断言
```

四项脚本全绿，就是这份对照表的证据。
