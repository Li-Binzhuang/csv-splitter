# xlsx 互操作 / Excel interop

状态：**导出已实现；导入尚未实现**。用户已解除 xlsx 禁用；仍然不做前端。

## 导出

```bash
csvx convert orders.csv --format xlsx -o orders.xlsx
```

- 流式读取 CSV，边读边写 `xl/worksheets/sheet1.xml`，不把整表放进内存；
- 首行加粗、冻结首行；
- 数字写成数值单元格；`true/false` 写成布尔；其余为 inline string；
- 前导零（如 `007`）保持文本，不被 Excel 吃掉；
- 超过 Excel 单表上限 **1,048,576 数据行**直接报错，不静默截断；
- sheet 名取文件名并清洗非法字符；
- ZIP/OOXML 由项目自己写；没有新增 xlsx 依赖。

MCP：

```json
{"name":"csvx_convert","arguments":{"path":"orders.csv","format":"xlsx","out":"orders.xlsx"}}
```

## 已验证

- core 单测：ZIP 成员、XML 头、inline string、数值、前导零、多 sheet；
- CLI 单测：真实 `convert` 输出可由 ZIP 读取；
- 用 openpyxl 读取导出的文件，得到：

```text
('id', 'amount', 'flag', 'note')
(1, 12.5, True, 'a&b')
(2, '007', False, '<x>')
freeze A2
```

## 尚未实现

- 导入 xlsx（计划：只读缓存值、选 sheet、日期转换、忽略样式/合并）；
- 导出 xlsx 的多 sheet 报告（`report --format xlsx`）；
- 条件格式/列宽/数字格式；
- 公式写入（只写值，不写公式）。

## 边界

- 不做宏/VBA；
- 不重算 Excel 公式；
- 不把 xlsx 作为内部存储格式；CSV 仍是源格式。
