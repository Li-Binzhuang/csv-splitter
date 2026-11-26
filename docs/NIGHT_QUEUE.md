# Night Queue / 待用户决策

夜间静默窗口不自行决定、留到白天的事项：

- xlsx 格式是否解禁；
- xlsx 读路径是否迁移到 calamine：涉及新增 crate、离线 vendor、License/供应链审查；方案见 `docs/XLSX_ENGINE.md`；
- 目标演进：`update_goal edit` 需要直接人类轮次，当前 goal 仍停在 xlsx 导入审计（revision 8）；
- xlsx 是否做流式解析：1M×10 峰值 1.02 GB，当前整表读入；改造需拆分解析迭代器/宽度预扫，属较大重构；

- 商业化定价 / license / 遥测；
- 删除用户文件、不可逆迁移、force push；
- 需要产品取舍的界面/交互方向。
- 目标是否收尾：硬约束「无 GUI」与目标里的「薄前端」存在冲突；现有 opt-in 网页 viewer 是否算薄前端，需你白天裁决。两条路与验收见 `docs/THIN_FRONTEND_PLAN.md`。

> 夜间只记录，不执行；白天由用户决定。
