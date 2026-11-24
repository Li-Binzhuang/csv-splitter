# csvx 大计划：Excel 互操作 + Agent 原生后端

状态：active（2026-11-24，用户批准解除 xlsx 限制；仍然不做前端）。
这个计划只描述**可验收的后端工作**，不承诺 GUI、多用户鉴权或宏。

## 一句话目标

让 csvx 从“CSV 工作台”变成：

1. 能读写 Excel 用户的文件（xlsx）；
2. 任何 Agent 只要接 API 就能发现工具、创建会话、预览/应用动作、读取事件；
3. 模型与 Agent 不绑定：项目不保存模型密钥，只提供本地 API 与审计。

## 原则

- 本地单用户、loopback；不做账号体系。
- 不保存模型 key；内置 runner 只从环境变量/命令行读取。
- 写操作一律 preview → apply；Agent API 不绕过 journal。
- 不把 Agent 的自由文本当权限；动作必须匹配 operation 契约。
- 每阶段有测试、gate、AOCI、发布包 smoke；每轮最多一次 push。

## Workstream A：xlsx 互操作

### A1 导出（先做）

CLI：

```bash
csvx convert orders.csv --format xlsx -o orders.xlsx
```

- 首行表头加粗、冻结首行、列宽按样本；
- 字符串/数字/布尔/日期识别为单元格类型；
- 单文件单 sheet，sheet 名安全化；
- 超过 Excel 行上限（1,048,576 数据行）明确报错，不静默截断；
- 多 sheet：`convert parts/ --format xlsx -o book.xlsx`，每个 CSV 一张 sheet；
- ZIP/OOXML 由项目自己写，不新增解析依赖（复用已有 `zip`）。
- MCP：`csvx_convert` 的 `format` enum 增加 `xlsx`。
- 测试：ZIP 成员齐全、XML 可解析、关键 XML 片段、数值/日期单元格类型、超大行拒绝。

### A2 只读导入

```bash
csvx convert input.xlsx --format csv -o out.csv   # 规划中的 sheet 选择见下文
```

- 读取缓存值，不重算公式；
- 默认第一个 sheet，规划中提供按名字选择；
- 合并单元格、样式、图表、批注忽略并在 stderr 说明；
- 日期序列转 ISO；
- 空行跳过；
- 超过 CSV 输出上限/异常 XML 明确报错；
- 测试用固定 xlsx 夹具（含 inline/shared string、日期、空行、第二 sheet）。

### A3 多 sheet 报告导出（在 A1 后）

`report --format xlsx`：Summary / Data / Chart 各一 sheet，值来自现有 report spec；
不做公式、不做图表对象嵌入，只写数值与样式。

### A4 基础格式

表头加粗、冻结首行、列宽、数字格式；条件格式等试点反馈后再做。

## Workstream B：公式结果缓存

现状：公式读时求值，重复读同一窗口会重算。

- key = 文件身份 + journal 当前块版本 + 公式列定义；
- 只缓存只读视图；任何 journal 变化立刻失效；
- 目标：重复读同一窗口稳定 < 2 ms（以实测为准，不先承诺）；
- 测试：命中、失效、编辑后不可复用、撤销后旧缓存不复用。

## Workstream C：Agent API（无前端）

### 协议

基础路径 `/agent/v1`，JSON；SSE 用于事件。所有动作映射到现有 operations/tools，不发明第二套语义。

| 方法 | 路径 | 作用 |
|---|---|---|
| GET | `/agent/v1/manifest` | 协议版本、工具清单、权限/限制、模型无关说明 |
| POST | `/agent/v1/sessions` | 创建会话：root、agent 名称/版本、允许的操作 |
| GET | `/agent/v1/sessions/{id}` | 会话状态、审计摘要 |
| POST | `/agent/v1/sessions/{id}/actions` | 执行一个动作（read/preview/apply/job） |
| GET | `/agent/v1/sessions/{id}/events` | SSE：action 开始/输出/审批/完成 |
| POST | `/agent/v1/sessions/{id}/approvals` | 审批 destructive 动作 |
| POST | `/agent/v1/sessions/{id}/close` | 关闭并落审计 |
| GET | `/agent/v1/audit?session=` | 审计记录（本地 JSONL） |

动作信封：

```json
{
  "requestId": "req-...",
  "operation": "edit.apply",
  "arguments": {"path": "t.csv", "ops": "set:1:qty:9"},
  "mode": "preview"
}
```

- `mode=read` 只允许只读 operation；
- `preview` 返回 preview，不写；
- `apply` 需要 `approvalId`（破坏性操作）且 root 内；
- `job` 返回 job id，事件流继续。
- manifest 输出同一份 operations 契约 + 23 工具，不复制 help 文本。

### 会话与安全

- `POST /sessions` 生成一次性 Bearer token，只在创建响应返回一次；
- token 绑定 root、允许的 operations、过期时间；
- 审计记录到 root 下 `.csvx-agent/audit.jsonl`，不含 token/模型 key；
- 所有路径 canonical + 必须在 root；
- 不新增多用户角色；token 只是单用户本地能力边界。

### 内置 Agent（可选、后端）

```bash
csvx agent run   # 通过环境变量或命令参数提供 endpoint/model/prompt；尚未实现
```

- 只从 env/命令行读取 key，不写盘；
- 循环调用模型 → 解析 tool call → 执行同一 actions → 回灌结果；
- 默认只跑 read/preview；destructive 必须显式确认（规划中的 allow-apply）；
- 用本地 DSH 的 **DeepSeek-V4.1-Flash-1017-Ark-Economy** 做测试模型，不接其他模型。

### DSH 接入

- 提供 `.mcp.json` 样例 + skill 说明，让 DSH 直接发现 csvx MCP；
- 不修改用户的 DSH profile，除非用户明确要求；
- 用本地 DSH 的 OpenAI-compatible 网关验证一次模型调用（只允许 Ark Economy）。
  已实测：`DeepSeek-V4.1-Flash-1017-Ark-Economy` 在线，1.10 s 返回 `OK`；
  记录见 `docs/DSH_MODEL_TEST.md`。

## Workstream D：项目整理

- 新增 `docs/AGENT_API.md`（协议）、`docs/XLSX.md`（互操作）、`examples/agent/`（curl/SDK 示例）；
- 现有 docs 不重复内容，只链接；
- 新增测试夹具目录 `tests/fixtures/xlsx/`；
- 发布包加入 Agent API 文档与示例；MCP 工具数量变化时同步 demo。

## 里程碑

| 阶段 | 交付 | 验收 |
|---|---|---|
| M1 | xlsx 导出 | CLI/MCP 输出可被 ZIP/XML 校验；测试 + 包 smoke |
| M2 | xlsx 导入（已落） | 固定夹具 + sheet 选择 + 日期/空行 |
| M3 | 公式缓存 | 命中/失效测试 + 实测重复读 |
| M4 | Agent API | manifest/session/action/approval/audit + SSE 测试 |
| M5 | 内置 runner | 本地 DSH Ark Economy 跑通一次 read→preview→apply 流程 |
| M6 | 整理与发布 | docs、examples、包、gate、AOCI |

## 不做什么

- 桌面 GUI / 网页前端；
- 多用户鉴权、SaaS、遥测；
- 宏/VBA、xlsx 公式重算、图表对象嵌入；
- 自动上传模型 key 或云端存储。
