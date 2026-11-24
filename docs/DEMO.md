# 端到端演示（无 GUI / 无网络）

一条命令演示两个可卖点：**声明式 Pipeline** 与 **MCP agent 集成**。

```bash
BIN=target/release/csvx bash scripts/demo_end_to_end.sh
```

实际输出：

```text
== csvx version ==
csvx 4.2.0 (DuckDB: compiled in)
== pipeline ==
✓ filtered (15 ms)
✓ quality (15 ms)
== MCP client ==
tools=16
schema_rows=5
filter_matched=3
gzip_matched=3
status_fresh=2
view_rows=2
edit_ok=True
convert_ok=True
job=done
== MCP official SDK ==
sdk_tools=19
schema_rows=5
filter_matched=3
view_rows=2
convert_ok=true
pipeline_progress_events=2
status_fresh=2
gzip_matched=3
progress_events=1
structured_ok=true
job=done
== accepted ==
```

> demo 会检查 release 二进制是否比 `crates/*/src/**/*.rs` 旧；旧则拒绝运行，避免演示结果与源码不符。
> 需要自动重建时用 `CSVX_DEMO_BUILD=1 bash scripts/demo_end_to_end.sh`。

## 演示覆盖
1. `pipeline run`：filter → quality，列出每步耗时；`.pipeline.csv` 记录血缘，重跑会跳过。
   随后 `pipeline watch --iterations 1 --json --state` 跑一轮，验证 JSON 与状态文件。
2. `scripts/mcp_client.py`：真实 stdio JSON-RPC 客户端，不依赖第三方库：
   - `tools/list`（18 个工具，18 条 annotations）
   - `csvx_schema` / `csvx_filter` / `csvx_view`
   - `csvx_edit_set` → `csvx_edit_undo`（journal 锁）
   - `csvx_convert` 输出 TSV
   - `csvx_job_submit` quality → `csvx_job_status` 直到 done
   - `csvx_cache_stats` / `csvx_cache_clear`（两次 gzip 后命中 1 条、清空并复查为 0、死进程清扫数）
3. 若 `scripts/mcp_sdk/node_modules` 存在，demo 还会跑官方 SDK 客户端：
   - `sdk_annotated=18`（SDK 看到 annotations）
   - `pipeline_progress_events=2`（pipeline 每步通知；官方 SDK 的 `onprogress` 偶发
     合并最后一条，实测 10 次中 1 次为 1；raw MCP 10/10 稳定 2 条，demo 只要求 ≥1）
   - `status_fresh=2`（MCP `pipeline_status` 带 `contentCheck` 核验）
   - `gzip_matched=3`（MCP `search` 对 `.gz` 输入开启 `materialize`）
   - `cache_hits` / `cache_entries` / `cache_removed` / `cache_stale` / `cache_after=0`（cache_stats/clear）
   - `progress_events=1`（job wait 通知）
   - `structured_ok=true`（structuredContent 成功码）
4. 全程本地、无 GUI、无网络、无 xlsx，不写遥测。

## 官方 MCP SDK 兼容性

用 `@modelcontextprotocol/sdk` **1.32.2**（官方 TypeScript SDK）的 `Client` +
`StdioClientTransport` 跑同一闭环：

```bash
cd scripts/mcp_sdk && npm install
CSVX_BIN=../../target/release/csvx CSVX_ROOT="$DIR" CSVX_FILE=orders.csv node client.mjs
```

实际输出：

```text
sdk_tools=19
schema_rows=3
filter_matched=2
view_rows=2
convert_ok=true
pipeline_progress_events=2
progress_events=1
structured_ok=true
job=done
exit=0
```

说明：csvx 的 MCP server 与官方 SDK 的 initialize/tools/list/tools/call/job 流程兼容；
npm 依赖只在演示目录，不进入 Rust 构建。

## 适合谁看
- 数据工程：把 100 GB CSV 的搜索/过滤/导出当本地数据库用；
- 运营/财务：分页查看、编辑、撤销、质量报告；
- Agent/IDE：MCP 工具可直接把“搜索→定位→编辑→导出”闭环交给模型。

更多性能数字见 `docs/BENCHMARKS.md`；商业化假设见 `docs/BUSINESS.md`。
