# 验收清单 / Acceptance

> 目标：按 Operations 契约路线，把 csvx 演进为本地 AI 原生 CSV 工作台。
> 日期：2026-11-24。所有数字来自本机 release 构建或发布包实测。

## 一键复现

```bash
bash scripts/gate.sh                 # 全量测试 + clippy + docs
bash scripts/demo_end_to_end.sh      # CLI + MCP SDK 端到端
python3 scripts/smoke_http.py        # 发布包 HTTP 面
bash scripts/package_release.sh      # 打包并 verify_dist
```

## 里程碑

| 里程碑 | 证据 | 结果 |
|---|---|---|
| Stage 2 共享操作目录 | `csvx ops --json` / `GET /ops` / MCP `csvx://operations` | 17 操作，三端同一份 |
| Stage 2.5 typed operations | `SchemaRequest` / `FilterRequest` / `ViewRequest`；HTTP `/schema` `/filter` `/rows` | schema/filter typed CLI+HTTP；view core + HTTP 简单无编辑窗口；复杂 where/order 仍 SQL |
| Stage 3 preview/apply | `csvx_edit_preview` / `preview_edit` / `EditApplyRequest` | CLI/HTTP/MCP preview；CLI/HTTP 共用 typed apply，MCP 走同一 CLI 路径 |
| 压缩输出 | `filter -o out.csv.gz` | 56 MB → 7.3 MB，0.40 s |
| Pipeline 质量门禁 | `gate` 单文件/目录，CLI/HTTP/MCP | 失败 JSON 带 `failures`；HTTP 200 + `passed:false`；MCP `isError` |
| 多文件/目录 | `concat` CLI/HTTP/MCP；`gate` 目录 | HTTP 目录按文件名排序并排除输出；4M 行 concat 0.59 s（MCP） |
| 跨文件唯一性 | `gate --unique-global` / `uniqueGlobal` | 4M 行、2 文件：7.03 s，失败原因可读 |
| 报表 spec | `report` CLI/HTTP/MCP | summary/columns/quality/groups/chart SVG/compare/rows；内联 `specText` |
| 报表安全 | HTTP/MCP `--root` | compare 第二文件路径不能越 root |
| 发布包 HTTP | `smoke_http.py` | 6/6 检查通过，errors=[] |

## 约束核对

- 本地单用户：无登录、无多用户鉴权、无遥测。
- 无 GUI：未新增桌面/图形界面；报表是可打开的 Markdown/HTML 静态文件。
- xlsx：未启用。
- 网络：核心命令与 MCP 不依赖网络。
- 计费：未接入 license/计费。

## 尚未收尾

- **薄前端**：与“无 GUI”硬约束存在范围冲突；现有 opt-in 网页 viewer 是否算薄前端，需用户裁决（见 `docs/NIGHT_QUEUE.md`）。
- **xlsx / 数据库只读连接**：需用户明确解除约束后才能做。

## 当前产物

- MCP 工具：23。
- AOCI：123 entries, aligned。
- 包：`dist/csvx-4.2.0-darwin-arm64.tar.gz`，以最新构建 SHA 为准。
