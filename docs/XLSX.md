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
  --no-freeze --no-auto-filter \
  --data-validation "status:good,ok,bad" \
  --data-validation-prompt "status:Status|Choose one" \
  --data-validation-error "status:Bad status|Pick good or ok" \
  --data-validation-blank \
  --protect-sheet \
  --col-width "amount:20" \
  --protect-sheet-password test \
  --protect-sheet-allow-objects \
  --data-validation-formula "amount:AND(B2>0,B2<100)" \
  --data-validation-whole "amount:0,1200" \
  --data-validation-decimal "id:0.5," \
  --data-validation-error-style "status:warning" \
  --number-format-raw "amount:[Blue]#,##0.00" \
  --data-validation-range "status:$A$2:$A$10" \
  --data-validation-date "day:2024-01-01,2024-12-31" \
  --data-validation-time "hour:09:00,17:00" \
  --validations \
  --formulas
```

- 规则用 `;` 分隔，每条 `列 操作符 值 : 颜色`；
- 操作符 `>` `<` `>=` `<=` `=` `!=`；
- 颜色 `red/amber/green/blue`；
- 写成 Excel `cellIs` 条件格式（`dxf` + `conditionalFormatting`），
  不是静态涂色；
- `--no-freeze` 省略表头冻结行；
- `--no-auto-filter` 省略表头筛选；
- `--data-validation` 写下拉列表，格式 `列:值1,值2,值3`；重复列/空列表/非 xlsx 退出 2；
- `--data-validation-prompt` 写选中提示，格式 `列:标题|正文`；列须已有验证；
- `--data-validation-error` 写错误提示，格式同上；
- `--data-validation-blank` 允许空白单元格通过验证；
- `--data-validation-whole` 写整数区间验证，格式 `列:最小值,最大值`（可单边留空）；
- `--data-validation-decimal` 写小数区间验证，格式同上；
- 整数/小数验证与列表、自定义公式互斥；提示/错误/空白开关对它们同样生效；
- `--data-validation-error-style` 指定错误对话框样式，格式 `列:stop|warning|information`；列须已有验证；
- `--data-validation-date` 写日期区间验证，`列:起始,结束`（ISO 日期，内部转 Excel 序列号）；
- `--data-validation-time` 写时间区间验证，`列:起始,结束`（`HH:MM`，内部转 0–1 天小数）；
- `--data-validation-range` 列表源用单元格区域引用，格式 `列:区域`（如 `$A$2:$A$10`），与内联值互斥；
- `--formulas` 读取 xlsx 的公式文本与缓存值（不做重算），可配 `--sheet`/`--all-sheets`/`--json`；
- `--validations` 读取 xlsx 的数据验证；日期/时间边界从序列号投影回 ISO/时分秒（列/类型/operator/公式/提示/错误样式），可配 `--sheet`/`--all-sheets`/`--json`；
- `--protect-sheet` 写工作表保护（对象/场景受限，不设密码）；
- `--number-format-raw` 原样保留任意自定义格式码到 xlsx `formatCode`（不做渲染解释，≤255 字符）；
- `--number-format` 支持颜色前缀（`[Red]`、`[Blue]` 等 8 色）并写入 formatCode；
- `--protection` 读取 xlsx 的 sheetProtection 四标志（可配 `--all-sheets`、`--json`），不产出文件；
- `--col-width` 指定列宽，格式 `列:宽度`，宽度 0.5–255；目录多 sheet 时每 sheet 应用同一宽度；
- `--number-format` 目录多 sheet 时每 sheet 应用同一列格式；
- `--protect-sheet-allow-objects` / `--protect-sheet-allow-scenarios` 放行对象/场景操作（objects/scenarios=0）；
- `--protect-sheet-password` 写 Excel 兼容的保护密码哈希（1–15 位 ASCII），可与 `--protect-sheet` 并用。
  缺失验证/缺 `|`/重复列/非 xlsx 均退出 2；
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
