# 产品进化计划 / Product Evolution Plan

状态：active（2026-11-24 起）  
定位：**本地优先、AI 原生、面向大文件的 CSV/表格工作台**。不是 Excel 的 1:1 替代，
而是把大文件、可审计编辑、确定性工具和 AI 编排组合在一起。

## 产品形态 / Product shape

```text
CLI  ─┐
HTTP ├─→ operations contract → core engine
MCP  ─┘         │
                ├─ read / preview / apply / job
                ├─ stable error codes
                └─ report spec
```

前端后置、CLI 先行；但 CLI/HTTP/MCP 从同一份契约出发，避免三套逻辑漂移。
AI 不直接改文件：先预览，再应用，可撤销。

## 阶段与验收 / Stages

| 阶段 | 目标 | 验收 | 状态 |
|---|---|---|---|
| S0 契约目录 | 操作名、效果、错误码可发现 | `csvx ops --json` | ✅ |
| S1 三端目录 | CLI/HTTP/MCP 同一份目录 | `GET /ops`、MCP `csvx://operations`、测试对齐 | ✅ |
| S2 typed operations | `schema/view/filter` 请求/响应类型进 core | schema/filter CLI/HTTP、view CLI 简单路径已接入；view HTTP 待接 | 进行中 |
| S3 preview/apply | 写操作先预览，应用失败不写半条 | `edit transaction --dry-run` 已落；CLI + HTTP + MCP preview 有 affected rows/hash | 进行中 |
| S4 数据能力 | 压缩输出、Pipeline 质量门禁、多文件/目录 | filter `-o` 压缩输出、`gate` 阈值门禁已落；多文件/目录待做 | 进行中 |
| S5 薄前端 | 虚拟网格、编辑/撤销、公式/筛选/透视 | 只消费契约，不解析 help | 待做 |
| S6 报表 | 声明式 report spec + 确定性渲染 | CSV spec → Markdown/HTML（summary/columns/quality/groups/rows）已落；前端渲染待 | 进行中 |
| S7 集成 | 数据库只读连接、xlsx（需用户解除） | 凭据/权限/回滚 | 条件待定 |

## 当前优先 / Now

1. **S2 typed operations**：先抽 schema/view/filter 的请求与响应；
2. **S3 preview/apply**：`edit_preview` 只算不写，`edit_apply` 写事务；
3. **S4 压缩输出**：filter/convert 写 `.gz/.zst`，2 GB 输出实测；
4. **S4 质量门禁**：Pipeline 行数/空值/唯一性阈值失败即停；
5. **S5 薄前端**：虚拟网格 + 现有 HTTP 端点。

## 硬约束 / Constraints

- 本地单用户；不做多用户鉴权；
- 不做 GUI（前端后置，不是禁止最终做）；
- xlsx 在用户明确解除前不做；
- 不引入 license 强制/遥测；
- 不承诺未实测数字；测试、gate、AOCI、包验证每轮收尾。

## 判断标准 / Done means

- 行为有测试钉住；
- 数字有实测；
- CLI/HTTP/MCP 行为一致；
- 做不到的部分写进文档，不藏在角落。

关联：[`OPERATIONS.md`](OPERATIONS.md)、[`ARCHITECTURE.md`](ARCHITECTURE.md)、
[`BEYOND_EXCEL.md`](BEYOND_EXCEL.md)、[`ROADMAP.md`](ROADMAP.md)。
