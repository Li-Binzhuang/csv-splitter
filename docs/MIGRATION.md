# 从现有脚本迁移到 csvx（本地试点）

> 目标：保留现有脚本的结果口径，把“读文件/过滤/核对”换成本地 csvx 命令或
> pipeline；不要求一次迁移全部流程。

## 1. 先留基线

1. 选一份脱敏样本；
2. 用现有脚本记录：输出文件、耗时、RSS、关键计数；
3. 用 `scripts/pilot_report.py` 对同一样本生成 csvx 报告；
4. 对照结果，不只看速度。

## 2. 最小迁移

| 现有动作 | csvx 动作 |
|---|---|
| 打开/看前几行 | `preview` 或 `view` |
| 条件过滤 | `filter` |
| 文本查找 | `search` |
| 去重/排序 | `dedupe` / `sort` |
| 列统计/质量 | `analyze` / `stats` |
| 多步流程 | `pipeline` 计划文件 |
| Agent 查询 | `mcp` 或 `agent serve` |

## 3. Pipeline 迁移

把脚本中的多步命令写成 CSV 计划：

```text
step,input,command,args,output
filtered,orders.csv,filter,--where city=CDMX -o {out},filtered.csv
quality,filtered,analyze,--quality --json,quality.json
```

先 `--dry-run`，再实际运行；`pipeline status --verify` 检查 fresh/stale。
失败时保留原脚本作为回滚路径。

## 4. Agent 迁移

1. 生成客户端配置：`csvx mcp config --client ...`（按客户端选择）；
2. 只把 root 指向脱敏目录；
3. 用 `csvx_schema` → `csvx_view` → `csvx_filter` 的顺序验证；
4. 编辑前先 `csvx_edit_preview`，再 apply；
5. 审计日志在 `.csvx/mcp-audit.jsonl`，不落原始参数值。

## 5. 回滚与删除

- 迁移不修改原文件；journal/undo 可回退编辑；
- 试点结束按约定删除样本、临时物化和报告；
- 任何收费/License 决策不在迁移阶段做。
