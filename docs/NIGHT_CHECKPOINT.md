# csvx 阶段检查点（2026-11-25）

本文件记录当前可验证状态，供白天复盘；不是营销材料。

## 已验证

| 项 | 实测 |
|---|---|
| MCP 工具 | 23 个，23 条 annotations，23 个 SDK 工具 |
| Agent HTTP 路由 | 12 条，manifest 与实际路由一致 |
| Agent 集成测试 | 10 个 |
| CLI 集成测试 | 170 个 |
| HTTP serve 测试 | 174 个 |
| SSE 首帧 | 1–3ms（demo 断言 < 250ms） |
| AOCI | 142 entries，aligned |
| 发布包 | `csvx-4.2.0-darwin-arm64.tar.gz`，哈希见 `dist/*.sha256` |

## 本轮已落地

- xlsx 严格 ISO 日期单元格 + 内置日期样式导入；
- xlsx 条件格式 `--highlight "amount>1000:red"`；
- report `xlsx` / `datecounts[,date[:group]]`；
- DSH skill `csvx-data-workbench` 安装并在 DSH 中加载；
- Agent API：session/actions/approvals/events/audit/trace/replay/curl/actions.md/html/runbook.md/JSON；
- 审批：403 deny、428 missing、428 expired（默认 5 分钟）、剩余 TTL；
- SSE：审计写入 condvar 唤醒、15s ping、实测首帧毫秒数；
- 多 session 汇总（不返回 token/内容）；
- demo 覆盖 CLI、MCP、SDK（可选）、Agent 路由、审批、重放、SSE；
- 发布包提供 `TRY_CSVX.sh` 五分钟本地试用。

## 已知边界

- 单用户、本机 loopback，无多用户角色；
- 无 GUI；
- xlsx 条件格式仅 `cellIs` 规则，不写公式/合并/样式复制；
- replay 是 CLI 侧执行，HTTP 只有导出；
- trace/audit 含数据，按敏感文件管理；
- key 只从环境变量读取，不写入 argv/磁盘/trace。

## 待白天决策

- xlsx 是否全面解禁（当前按 capability 渐进开放）；
- 商业化定价与分发方式；
- 是否引入多用户权限模型。
