# 数字格式覆盖矩阵

日期：2026-11-27 · 实现：`crates/core/src/layout.rs::NumberFormat` · 口径：格式是“对数字的承诺”，非数字/非日期值原样返回。

## 支持

| 格式码 | 状态 | 行为 | 测试 |
|---|---|---|---|
| `#,##0.00` / `#,##0` | 支持 | 千分位 + 固定小数位 | `number_formats_render_the_way_a_spreadsheet_does` |
| `0.00` / `0` | 支持 | 固定/零小数位，四舍五入 | 同上 |
| `0%` / `0.0%` | 支持 | 0.123 → 12.3%，乘 100 | 同上 |
| `$#,##0.00` / `¥` / `€` | 支持 | 货币符号 + 千分位 + 小数位 | 同上 |
| `yyyy-mm-dd` | 支持（新增） | ISO 日期 → 破折号日期 | `date_time_formats_render_iso_values` |
| `yyyy/mm/dd` | 支持（新增） | 斜杠日期 | 同上 |
| `dd/mm/yyyy` | 支持（新增） | 日/月/年 | 同上 |
| `dd-mm-yyyy` / `d-m-yyyy` | 支持（新增） | 破折号日/月/年 | `dotted_date_formats_match_excel_orders` |
| `yyyy-mm-dd h:mm[:ss] am/pm` | 支持（新增） | 12 小时制，AM/PM | 同上 |
| `dd-mm-yyyy h:mm[:ss] am/pm` | 支持（新增） | 12 小时制，日/月/年 | 同上 |
| `yyyy.mm.dd h:mm am/pm` | 支持（新增） | 12 小时制，点分隔 | 同上 |
| 点/破折号格式导出 | 支持（新增） | 写入 `numFmt formatCode` 与序列号单元格 | `dotted_and_dashed_formats_map_into_export_styles` |
| `m/d/yyyy` / `mm/dd/yyyy` 导出 | 支持（新增） | 斜杠日期/时间格式写入 numFmt | `slash_date_formats_map_into_export_styles` |
| `m/d/yyyy h:mm[:ss]` / `mm/dd/yyyy h:mm[:ss]` | 支持（新增） | 斜杠日期 + 24 小时时间 | `dotted_date_formats_match_excel_orders` |
| 斜杠 12 小时 am/pm | 支持（新增） | `m/d/yyyy h:mm[:ss] am/pm`、`mm/dd/yyyy ...` | 同上 |
| `h` vs `hh` 小时补零 | 支持（新增） | 单个 `h` 不补零（9:05），`hh` 补零（09:05） | `dotted_date_formats_match_excel_orders` |
| `yyyy-m-d` | 支持（新增） | 破折号年/月/日（不补零） | 同上 |
| `dd.mm.yyyy` / `d.m.yyyy` | 支持（新增） | 点分隔日/月/年 | 同上 |
| `yyyy.mm.dd` / `yyyy.m.d` | 支持（新增） | 点分隔年/月/日 | 同上 |
| `dd.mm.yyyy h:mm[:ss]` | 支持（新增） | 点分隔日期 + 时间 | 同上 |
| `mm/dd/yyyy` | 支持（新增） | 月/日/年 | 同上 |
| `m/d/yyyy` / `yyyy/m/d` | 支持（新增） | 不补零的月/日 | `date_time_formats_render_iso_values` |
| `yyyy-mm-dd hh:mm` | 支持（新增） | 日期 + 时:分 | 同上 |
| `yyyy-mm-dd hh:mm:ss` | 支持（新增） | 日期 + 时:分:秒 | 同上 |
| `hh:mm` / `hh:mm:ss` | 支持（新增） | 纯时间显示 | 同上 |
| `0.00E+00` / `0.0e+00` | 支持（新增） | 科学计数，指数补两位 | `scientific_accounting_and_text_formats_work` |
| `#,##0.00;(#,##0.00)` | 支持（新增） | 负数括号；可带 `$`/`¥`/`€` | 同上 |
| `@` | 支持（新增） | 文本原样显示 | 同上 |

输入约定：日期是 ISO 前缀文本（`2024-01-02` / `2024-01-02 13:04:05` / `T` 分隔），不是 Excel 序列号；序列号在 xlsx 导入阶段已转成 ISO。

## 缺失

| 能力 | 现在 | 影响 |
|---|---|---|
| 会计对齐 `_($* #,##0.00_)` | 支持（新增） | 解析 `_(符号* #,##0.00_)` 与裸 `符号* #,##0.00`；name 归一化为带 `_( )_`；渲染保留符号与括号 |
| 负数颜色段 `[Red]` | 支持（新增） | 8 种颜色前缀保留进 xlsx formatCode；渲染取内层格式 |
| 日期序列号显示 | 不支持 | 未覆盖的 Excel 扩展码 |
| 四位分数 `????/????` | 支持（新增） | `# ????/????` 与 `????/????`，分母上限 9999 |

| 任意 Excel 自定义格式码 | 只识别白名单 | 未识别格式会报 “not a number format”，不静默套用 |

## 本轮验收

```bash
cargo test -p csvx-core --lib layout::tests
scripts/gate.sh
```

新增测试 `date_time_formats_render_iso_values` 覆盖 8 种日期/时间格式、ISO `T` 分隔、非日期值原样返回、非法 `yyyy-mm` 拒绝。

## 与后续项的关系

- view/format 的解析与渲染已实现；xlsx 导出样式映射（`write_sheets_with_formats` / `--number-format`）已接线；
- `xlsx_column_formats` 已能把导入 workbook 的 header/首行样式读回 `NumberFormat`，尚未接到 CLI。
- 负数括号与 `_($* #,##0.00_)` 会计对齐已实现；多段会计码（含 "-" 占位）仍不支持。
