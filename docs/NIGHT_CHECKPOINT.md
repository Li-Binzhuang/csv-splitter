# Night Checkpoint / 夜间交接

> 用户次日 **10:00** 查看。夜间静默工作维护，不叫醒用户。

## 2026-11-24 夜间汇总

### 已完成并有测试/实测

- **Stage 2 操作目录**：CLI `ops --json`、HTTP `/ops`、MCP `csvx://operations` 共用 **17 个操作**。
- **Stage 2.5 typed**：schema/filter 的 CLI+HTTP typed 路径；view 的 core `ViewRequest` + HTTP `/rows` 简单无编辑窗口。
- **Stage 3 preview/apply**：CLI/HTTP/MCP 三端 preview；CLI/HTTP 共用 `EditApplyRequest`，MCP `csvx_edit_apply` 走同一 CLI 路径。
- **压缩输出**：filter 支持 gzip/zstd/bz2/xz；实测 56 MB → 7.3 MB gzip。
- **质量门禁**：gate 单文件/目录，CLI/HTTP/MCP 三端；失败 JSON 带 `failures`，HTTP 保持 200 + `passed:false`，MCP 转 `isError`；目录按文件独立套阈值；`--unique-global` / HTTP `uniqueGlobal` 可额外要求跨文件唯一。
- **多文件/目录**：concat CLI/HTTP/MCP，均支持目录；HTTP 目录按文件名排序并排除输出。
- **报表 spec**：CLI/HTTP/MCP；sections `summary/columns/quality/groups/chart（count/sum）SVG/compare/rows`；内联 `specText`；HTTP/MCP 下 `compare` 目标受 root 守卫。
- **安全**：report compare 的第二文件路径不能越 root；MCP concat 的 paths/dir/out 也受限。
- **发布验收**：`scripts/smoke_http.py` 用发布包自动验证 viewer、/ops、/report chart+compare、gate 文件/global、concat 目录；本次 7/7，errors=[]。

### 当前产物

- MCP 工具 **23**（23 annotations；demo 验证 23/23）。
- AOCI **123** entries，治理 aligned。
- 包：`dist/csvx-4.2.0-darwin-arm64.tar.gz`
  - SHA256 `300de540992cf74ab7f7b9544726c76fc4539f53fe243ed9064769b94168b02d`
- 最新代码：`2b33aba`（已 push）；夜间轮次继续在本地累积，下一轮 push。

### 待白天决策

- 薄前端：当前硬约束是 **无 GUI**，未实现；报表 HTML/SVG 已是静态产物，不依赖前端。
- xlsx：未解禁。
- 数据库只读连接：未接。
- 商业化/定价/遥测：未做。

> 夜间继续按 active goal 自主推进；遇到需用户拍板的事项只写入 `docs/NIGHT_QUEUE.md`。
