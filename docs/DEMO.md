# 端到端演示（无 GUI / 无网络）

一条命令演示两个可卖点：**声明式 Pipeline** 与 **MCP agent 集成**。

```bash
BIN=target/release/csvx bash scripts/demo_end_to_end.sh
```

实际输出：

```text
== pipeline ==
✓ filtered (12 ms)
✓ quality (11 ms)
== MCP client ==
tools=13
schema_rows=5
filter_matched=3
view_rows=2
edit_ok=True
convert_ok=True
job=done
== accepted ==
```

## 演示覆盖
1. `pipeline run`：filter → quality，列出每步耗时；`.pipeline.csv` 记录血缘，重跑会跳过。
2. `scripts/mcp_client.py`：真实 stdio JSON-RPC 客户端，不依赖第三方库：
   - `tools/list`（13 个工具）
   - `csvx_schema` / `csvx_filter` / `csvx_view`
   - `csvx_edit_set` → `csvx_edit_undo`（journal 锁）
   - `csvx_convert` 输出 TSV
   - `csvx_job_submit` quality → `csvx_job_status` 直到 done
3. 全程本地、无 GUI、无网络、无 xlsx，不写遥测。

## 适合谁看
- 数据工程：把 100 GB CSV 的搜索/过滤/导出当本地数据库用；
- 运营/财务：分页查看、编辑、撤销、质量报告；
- Agent/IDE：MCP 工具可直接把“搜索→定位→编辑→导出”闭环交给模型。

更多性能数字见 `docs/BENCHMARKS.md`；商业化假设见 `docs/BUSINESS.md`。
