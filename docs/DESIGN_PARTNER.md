# 设计伙伴试点支持（4 周）

> 目标：用一份脱敏样本，在 4 周内判断 csvx 是否适合伙伴的真实工作流。
> 本文不是报价或 SLA；定价、合同、License 均由双方人工确认。

## 1. 适用范围

- 本地单机、单用户；
- CSV/TSV/NDJSON/Parquet；
- 1–20 GB 切片优先，超大文件先给 1 GB；
- 不接收生产凭据、账号密码、未脱敏 PII。

## 2. 伙伴提供

1. 脱敏样本或 1 GB 切片；
2. 列说明与一个真实问题；
3. 当前工具链的基线（耗时/RSS/结果）；
4. 可接受的验收口径与删除时间。

## 3. 四周计划

| 周 | 动作 | 产出 |
|---|---|---|
| 1 | intake、脱敏、样本边界确认 | `kickoff.json` / `kickoff.md` / demo report |
| 2 | 用 `csvx pipeline` 复现问题，记录 baseline | pipeline 计划 + 运行结果 |
| 3 | 修正口径、补缺失能力、重跑 | 对照报告与差异说明 |
| 4 | 交付报告、go/no-go 结论 | `pilot-report.md` + 结论 |

## 4. 成功指标（建议，不是承诺）

| 维度 | 目标 |
|---|---|
| 正确性 | 与伙伴现有结果逐行/聚合对照一致 |
| 耗时 | 不超过现有工具 80%，或单项 <60 s |
| RSS | 大文件扫描单进程 <200 MB |
| 集成 | 一个 MCP 客户端完成一次查询 |
| 可复现 | 一条命令或一个 plan 重跑 |

达不到就明确写“不达标”，不做营销包装。

## 5. 本地 kickoff kit

```bash
csvx demo -o pilot-kit/demo
CSVX_PILOT_FILE=pilot-kit/demo/orders.csv \
  python3 scripts/pilot_report.py --binary target/release/csvx \
  --where city=CDMX --key id --group city --out pilot-kit/pilot-report.md
bash scripts/pilot_kit.sh   # 一键生成 demo + report + kickoff
```

`pilot_kit.sh` 只生成合成数据，不发网络请求；产物在 `pilot-kit/`：

- `demo/orders.csv`
- `pilot-report.md`
- `kickoff.json`
- `kickoff.md`

## 6. 数据边界

- 纯本地；没有遥测或上传路径；
- MCP/HTTP 访问受 root 与 token 约束；
- 试用结束按约定删除样本；
- 报告只含文件名、大小、耗时、RSS、结果计数，不含原始行内容。

## 7. 非目标

- 不做 SaaS、多用户鉴权、GUI；
- 不在本阶段实现收费门控、License 强制或自动续费；
- 价格、合同、SLA 需人工确认。

## 8. 交付物

- 一份可复现的 `pilot-report.md`；
- 对应 pipeline plan（若适用）；
- 明确的限制与不达标项；
- go/no-go 建议，最终由伙伴与 csvx 人工决定。
