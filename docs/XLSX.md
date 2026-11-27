# xlsx 互操作 / Excel interop

状态：**导出与只读导入均已实现**。用户已解除 xlsx 禁用；仍然不做前端。

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

多 sheet（一个目录 → 一个工作簿）：

```bash
csvx convert parts/ --format xlsx -o book.xlsx
```

每个 `.csv` 一张 sheet，sheet 名取文件名；目录内输出文件会被排除；sheet 名冲突
自动加后缀；每张 sheet 都有冻结首行和列宽。

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

## 只读导入

```bash
csvx convert input.xlsx --format csv -o out.csv
csvx convert input.xlsx --format csv --sheet Data -o data.csv
```

- 读取缓存值，不重算公式；
- 默认第一个 sheet；`--sheet` 按名称选择，找不到时退出 1；
- 支持 sharedStrings、inlineStr、布尔、数值、缓存公式值；
- 换行/引号字段通过 CSV 引号规则保留；
- 样式、合并单元格、图表、批注忽略；
- 内置日期/时间格式（numFmtId 14–22、45–47）按样式还原：14–17/45–47 日期，18–21 仅时间，22 日期+时间；非日期数值不猜。

MCP：

```json
{"name":"csvx_convert","arguments":{"path":"input.xlsx","format":"csv","out":"out.csv","sheet":"Data"}}
```

## 条件格式

```bash
csvx convert data.csv --format xlsx -o data.xlsx \
  --highlight "amount>1000:red;status=bad:amber" \
  --no-freeze --no-auto-filter
```

- 规则用 `;` 分隔，每条 `列 操作符 值 : 颜色`；
- 操作符 `>` `<` `>=` `<=` `=` `!=`；
- 颜色 `red/amber/green/blue`；
- 写成 Excel `cellIs` 条件格式（`dxf` + `conditionalFormatting`），
  不是静态涂色；
- `--no-freeze` 省略表头冻结行；
- `--no-auto-filter` 省略表头筛选；
- 目录→workbook 不支持 `--highlight`。

## 尚未实现

- 合并单元格、样式复制；
- 其他自定义数字格式（分数 `# ?/?`、`?/?`、百分比 `#,##0.0%`、经过时间 `[h]:mm:ss` 已支持）；
- 公式写入（只写值，不写公式）。

## 报表 xlsx

`csvx report data.csv --spec spec.csv --format xlsx -o report.xlsx` 支持
`summary`、`columns`、`rows`、`datecounts` 四类 section，各写一张 sheet；
`datecounts,base_time:status` 生成日期 + 第二维，例如每个日期各状态的请求数；`quality`、`groups`、
`chart`、`compare` 仍走 Markdown/HTML。`--out` 必须以 `.xlsx` 结尾（或用
`--format xlsx` 显式指定）。

## 日期

严格 `YYYY-MM-DD` 自动写成 Excel 日期单元格（内置格式 14），`YYYY-MM-DDTHH:MM:SS` 写成带时间小数的日期，`HH:MM[:SS]` 在时间格式列写成当天小数；导入时按 `cellXfs` 的日期/时间格式还原；`@` 文本格式写成带样式 inlineStr，非 ISO 文本保持原样；`[h]:mm:ss` 经过时间格式写成累计小时序列（1.5 = 36:00:00），导入保持数值不猜日期。

## 边界

- 不做宏/VBA；
- 不重算 Excel 公式；
- 不把 xlsx 作为内部存储格式；CSV 仍是源格式。
