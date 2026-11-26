# 支持矩阵与工单模板（本地部署）

> 本文不是 SLA 承诺。支持范围以实际发布包与本机验证为准；定价/合同另行确认。

## 1. 平台矩阵

| 平台 | 架构 | 状态 | 说明 |
|---|---|---|---|
| macOS 27.2.1 | arm64 | 已实测 | 本仓 4.2.0 发布包、100GB benchmark、安装自检、升级/回滚 |
| macOS 其他版本 | arm64 | 未逐版本验证 | 需用同一发布包跑安装自检和 smoke |
| Linux | x86_64 | 未在本轮验证 | 需在目标机重新构建/打包，不直接复用 macOS 包 |
| Windows | x86_64 | 不支持 | 无发布包、无安装脚本 |

发布包是平台相关的；不要把 macOS 包复制到 Linux 使用。

## 2. 功能矩阵

| 能力 | 状态 | 验证入口 |
|---|---|---|
| CLI 预览/过滤/搜索/统计/编辑/undo | 可用 | `scripts/release-check.sh`、gate |
| Pipeline 计划/历史/重试/stale | 可用 | pipeline 测试、100GB 实测 |
| MCP 23 工具 + 3 资源 | 可用 | `scripts/check_mcp_sdk.sh` |
| HTTP Agent API 12 路由 | 可用 | `scripts/agent_routes_smoke.py` |
| 离线安装/自检/升级回滚/诊断包 | 可用 | `scripts/install_selfcheck.sh`、`scripts/ops_bundle.sh` |
| xlsx 转换 | 可用 | release 验证与 gate |
| 压缩 CSV（gz/zst/bz2） | 可用 | 压缩 intake 与 MCP 缓存 smoke |
| 多用户/权限/租户 | 不支持 | 单机单用户 |
| 托管 SaaS/GUI | 不支持 | 本地工具 |

## 3. 数据边界

- 试点样本必须脱敏；
- 报告只保留文件名、大小、计数与耗时，不保留原始行；
- 诊断包默认不含审计/历史日志，显式包含时也只含已脱敏 JSONL；
- 支持工单不要附生产凭据、token、完整客户表。

## 4. 工单最小信息

```text
partner: P-xxx
platform: macOS/Linux + arch + version
csvx version: <csvx --version>
install method: offline tar / source
command (no secrets): ...
expected: ...
actual: ...
diagnostic bundle: yes/no
```

附上 `ops-bundle.tar.gz` 与它的 `.sha256`，不要附原始 CSV。

## 5. 升级/回滚支持

按 `docs/DEPLOY.md` 的双 prefix 流程；回滚前先跑旧 prefix 自检。失败时保留旧 prefix
与诊断包，不在工单里粘贴完整环境变量。

## 6. 响应边界

- 本地单机、best effort，无 SLA；
- 平台/架构不匹配的包不承诺可运行；
- 需要合同、发票或 SLA 的事项由人工确认。
