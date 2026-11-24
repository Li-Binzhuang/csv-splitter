# 操作契约层 / Operations Contract

状态：**Stage 1 已完成；Stage 2 目录部分已落地**（2026-11-24）  
范围：`crates/core/src/contract.rs`、`csvx ops --json`、`GET /ops`、MCP `csvx://operations`

## 问题 / Context

同一件事有三套入口：

- CLI：人/脚本使用，参数是字符串；
- HTTP：前端使用，参数是 URL/JSON；
- MCP：AI 使用，参数是 JSON Schema。

目前它们都调用 core，但各自的名称、效果、错误码和参数说明是分开写的。结果是：

- 同一个操作三种叫法，客户端难以发现；
- 错误码靠字符串约定，改一处可能让另一处失配；
- 前端/AI 只能读 `--help` 或 `/spec`，拿不到统一的“能做什么”目录；
- 写操作缺少统一的 preview/apply 语义，AI 容易直接落盘。

## 决定 / Decision

在 core 建立**适配器无关的操作目录**，逐步演进为 typed operations：

```text
CLI adapter ─┐
HTTP adapter ├─→ core::contract / core::ops
MCP adapter ─┘        │
                      ├─ name + effect (read/preview/apply/job)
                      ├─ stable error codes
                      ├─ input/output contract
                      └─ typed request/response (Stage 2)
```

原则：

1. **名称稳定**：目录里的 `name` 是公开身份，CLI/HTTP/MCP 不再各自取名。
2. **效果显式**：`read` 不写文件；`preview` 只算不写；`apply` 可写；`job` 返回后仍在运行。
3. **错误码稳定**：HTTP 状态可以映射，错误码字符串不改名。
4. **写操作先预览**：AI 和前端先拿 preview/affected 范围，再 apply。
5. **契约优先**：Stage 2 的 JSON Schema 从 Rust 类型生成；在此之前，目录里的 `input/output` 只是摘要，不冒充完整 Schema。

## 当前目录 / Current catalog

`csvx ops --json`、`GET /ops` 与 MCP `csvx://operations` 输出同一份：

```json
{
  "version": "4.2.0",
  "operations": [
    {"name": "schema", "effect": "read", "summary": "...", "input": "path", "output": "columns, rowCount"}
  ],
  "errorCodes": ["invalid_request", "not_found", "path_escape", "..."]
}
```

已登记 17 个操作：`schema/view/filter/search/quality/pivot/chart/edit_preview/edit_apply/convert/pipeline_run/pipeline_watch/job_status/cache_stats/gate/report/concat`。

这还不是完整的命令清单；先覆盖前端与 AI 最常用的读/预览/应用/任务四类，后续按适配器迁移逐个补齐。

## 迁移计划 / Migration

**Stage 1（本次）**
- 目录 + 错误码 + 效果 + CLI `ops`；
- 单元测试锁住唯一名称与 JSON 结构。

**Stage 2（目录部分已完成）**
- HTTP `GET /ops` 与 MCP `csvx://operations` 返回同一份 `operations_json()`；
- 已新增 `core::ops` 的 `SchemaRequest/Response`、`ViewRequest/Response`、`FilterRequest/Response`；
- `schema` 的 CLI 与 HTTP 已改用 `SchemaRequest`（含 pristine sample / edited response）；
- `filter` 的 CLI 与 HTTP 已改用 `FilterRequest`（CLI `execute_fast`，HTTP `execute_page` 保留 cursor/预算）；
- `view` 的 CLI 简单路径（无 order/where/find）已改用 `ViewRequest`；
- 剩余：`view` 的 HTTP `/rows` 与复杂 CLI 路径；
- CLI 只做字符串到类型的适配。

**Stage 3（已开始）**
- `edit transaction --ops ... --dry-run`：完整解析并对照真实行/列校验，打印 `operations/rows/applied:0`，**不创建 journal**；
- preview 现在返回 `rowsBefore/rowsAfter/rowDelta/affectedRows/affectedRanges`、
  `ruleOperations`、`structuralOperations` 与 journal hash（FNV，非 SHA）；
- CLI `--dry-run`、HTTP `POST /edit?...&preview=1` 与 MCP `csvx_edit_preview` 共用 `preview_edit`；
- 下一步：preview 的“应用后内容指纹”，以及 `apply` 的失败回滚测试；
- 所有错误从 `ErrorCode` 映射，禁止新增平行字符串。

**Stage 4（已开始）**
- `filter -o out.csv.gz/.zst/.bz2/.xz` 压缩输出：先流式写 plain，再压缩并删除中间文件；`.zip` 明确拒绝；
- `gate --min-rows/--max-empty/--unique` 质量门禁；唯一性按列外部排序后相邻比较，不受内存 cap；在 plan 中作为普通步骤即可失败即停；
- `concat <dir>` 目录输入：只取目录内 `.csv`，按文件名排序，输出默认在目录旁；
- `gate <dir>` 目录级门禁：每个文件独立执行阈值，汇总 JSON；任一失败退出 1；
- `report --spec/--spec-text` 用 CSV spec 渲染确定性 Markdown/HTML；支持 summary/columns/quality/groups/rows；HTTP `GET /report`（spec 或 specText）与 MCP `csvx_report` 返回同一份正文；
- JSON Schema 由 Rust 类型生成，接入 MCP `inputSchema` 与 `docs/API.md`；
- 前端只消费契约，不解析 `--help`。

## 验收 / Acceptance

- `csvx ops --json`、`GET /ops`、MCP `csvx://operations` 返回同一版本/同一操作集合；
- 任一操作的 effect 变更会同时被 CLI/HTTP/MCP 测试发现；
- 错误码改动必须过契约测试；
- `preview` 不产生 journal/output 字节；`apply` 失败不写半条。

## 非目标 / Non-goals

- 不在 Stage 1 重写所有 handler；
- 不引入 serde/async runtime 只为契约；
- 不让目录里的 `input/output` 摘要冒充 JSON Schema；
- 不做多用户鉴权/GUI/xlsx。

## 关联 / Links

- [`docs/ARCHITECTURE.md`](ARCHITECTURE.md) 的分层与模块地图；
- [`docs/BEYOND_EXCEL.md`](BEYOND_EXCEL.md) 的能力边界；
- `crates/core/src/contract.rs`。
