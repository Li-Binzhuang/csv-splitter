# 薄前端方案 / Thin Frontend Plan

状态：**待用户裁决**，未实现。硬约束「无 GUI」与目标里的「薄前端」存在冲突。

## 现状

- `serve` 提供 opt-in viewer：`GET /`（`crates/cli/src/web/index.html`）；
- 发布包 smoke 验证 `GET /` 200，但未把它作为产品界面推广；
- 数据面已经齐全：`/ops`、`/rows`、`/edit`、`/report`、`/gate`、`/concat`、`/job`。

## 两条路

### A. 保持 API-only（当前）

- 不新增前端；
- 用户用 CLI/MCP/HTTP 或自己接前端；
- 目标里的「薄前端」按硬约束解释为**不做**；
- 零额外维护成本，符合「本地单用户、无 GUI」。

### B. 解禁 opt-in 网页薄前端

只做一个**由 `serve` 提供的本地网页**，不是桌面 GUI：

- 虚拟滚动表格（`/rows` 分页/窗口）；
- 单元格编辑走 preview → apply，撤销/重做走已有 journal；
- 报表页消费 `/report?format=html`，不重复渲染逻辑；
- 质量门禁页消费 `/gate`，显示 `passed/failures`；
- 不内嵌公式引擎、不做 xlsx、不做多用户；
- 所有状态仍在本地文件与 journal。

验收：

1. `GET /` 打开即可看到表；
2. 深偏移不阻塞（复用 `/rows` 的窗口/缓存）；
3. 编辑必须预览后 apply，失败不写；
4. 撤销/重做与 CLI 共享 journal；
5. 不新增网络依赖、登录或遥测。

## 建议

若「薄前端」确实是目标的一部分，选 **B**，但把「无 GUI」重新界定为
「不做桌面 GUI，允许 serve 自带的本地网页」。若不改约束，选 **A**，并在
`docs/ACCEPTANCE.md` 里把 S5 标为 intentionally out of scope。

## 决策

见 `docs/NIGHT_QUEUE.md` 的薄前端条目；白天由用户裁决后，再动
`crates/cli/src/web/index.html` 与对应 HTTP 测试。
