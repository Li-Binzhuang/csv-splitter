# XLSX 读写引擎决策（本地后台优先）

> 结论：目前读路径仍是自写最小解析；面向“Excel 99%”目标，建议把读路径迁到
> calamine，写路径保留。迁移需要离线依赖/供应链审批，尚未执行。

## 1. 先澄清“官方库”

- 微软没有发布 Rust 版 Excel/OpenXML SDK；Excel 官方接口是 COM/OpenXML（.NET/Java 等）。
- `calamine` 是社区维护的 Rust 读取库（MIT），不是微软官方；`rust_xlsxwriter` 是社区写库。
- 因此不存在“用官方库”这个选项；只有“用成熟社区库”与“自写最小解析”的取舍。

## 2. 当前实现

`crates/core/src/xlsx.rs`：

- 写：ZIP + OOXML 最小写出，流式单 sheet，表头冻结、列宽、条件格式、日期；
- 读：共享字符串、inlineStr、缓存公式值、内置 + 常见自定义日期、sheet 大小写、
  合并左上值、空行跳过、16384 列 / 1,048,576 总行（1,048,575 数据行）上限；
- 错误：无缓存公式值、坏共享索引、超行/列均显式报错，不静默造值；
- 公式：`xlsx_formulas` 读回公式文本与缓存值（不重算）；
- 验证：列表/自定义公式/整数与小数区间（`type=whole|decimal`，between 或单边）；
  errorStyle 三态 stop/warning/information；回读 `xlsx_data_validations`；
  `--validations` 检测模式（sheet/all-sheets/json）；列表源可为内联值或区域引用；
- 保护：密码哈希、objects/scenarios 粒度、`xlsx_sheet_protection` 与 `xlsx_protection_map` 读回；
  检测健壮性矩阵 `convert_protection_robustness_matrix`（未知 sheet/默认首表/JSON 顺序/非 xlsx/损坏 ZIP/空工作簿）。

## 3. 为什么曾经自写

- 早期只需要最小读取，避免新增解析依赖和离线包体积；
- 错误语义必须可控（不能把缺失缓存公式值变成空字符串）；
- 现有 `zip` 已能读容器，手写只补 XML 子集。

## 4. 为什么现在建议 calamine

| 维度 | 自写解析 | calamine |
|---|---|---|
| 共享字符串/日期/合并 | 子集，需自行补 | 覆盖更广、成熟 |
| xlsx 变体/容错 | 每个边界自己维护 | 社区长期打磨 |
| 错误语义 | 完全可控 | 需要适配层 |
| 依赖/离线包 | 无新增 | 需 vendor + License/供应链审查 |
| 写路径 | 已有、测试覆盖 | 不必替换 |

## 5. 迁移方案（待批准）

1. 只替换读路径：`read_xlsx_to_csv` 内部改用 calamine；
2. 保留外层错误映射：无缓存公式值、坏索引、行列上限仍明确报错；
3. 写路径继续用现有 `xlsx.rs`；
4. 把 calamine 及其依赖 vendor 进离线包，更新 `verify_dist`/release manifest；
5. 用现有 fixture 矩阵回归：共享字符串、日期/日期时间、合并、空行、sheet 大小写、
   无缓存公式、超限。

## 6. 当前状态

- 迁移**未执行**：新增 crate 属于离线依赖/供应链决策，等用户明确批准；
- 自写路径继续维护，新增边界测试先钉住行为；
- 不新增前端，商业化（定价/License/企业包）暂缓。

## 7. 当前覆盖矩阵（2026-11-26）

| 输入/边界 | 当前行为 | 测试 |
|---|---|---|
| 共享字符串 | 索引解析，越界报错 | `reads_shared_strings_and_cached_formula_values`、`a_missing_shared_string_index_is_an_error_not_an_empty_cell` |
| 缓存公式值 | number/string/boolean 读缓存值 | `reads_shared_strings_and_cached_formula_values`、`cached_string_and_boolean_formula_values_are_read` |
| 无缓存公式值 | 明确报错，不写空值 | `formula_without_a_cached_value_is_an_error`、CLI `convert_reports_a_formula_without_a_cached_value` |
| 内置日期样式 | serial → `YYYY-MM-DD` | `iso_dates_are_written_as_excel_date_cells` |
| 1900 闰日 | serial 60 输出 Excel 的 1900-02-29 假闰日 | `date_serials_carry_time_fractions_and_time_only_cells` |
| 1904 日期系统 | `workbookPr date1904` 按 1904 纪元换算 | `date_serials_carry_time_fractions_and_time_only_cells` |
| 自定义日期/时间 | 常见 token；日期时间/纯时间 | `custom_date_number_formats_are_recognized`、`date_serials_carry_time_fractions_and_time_only_cells`、`custom_time_formats_are_recognized_and_render_as_clock` |
| 合并单元格 | 只输出存储的左上值，不展开 | `merged_cells_keep_the_stored_top_left_value_without_inventing_others`、CLI `convert_keeps_a_merged_cell_top_left_only` |
| 空行 | 跳过 | `table_from_rows_skips_empty_rows_and_enforces_the_limit`、CLI `convert_skips_empty_xlsx_rows` |
| 冻结/筛选开关 CLI | `convert --no-freeze`、`--no-auto-filter` 单/多 sheet 生效 | `header_freeze_can_be_disabled` + `multi_sheet_export_can_skip_header_freeze` + `options_can_disable_freeze_and_filter_with_formats` |
| 表头冻结开关 | 默认冻结首行；`csv_to_xlsx_without_freeze` 省略 `<pane>` | `header_freeze_can_be_disabled` |
| 数据验证下拉 | `csv_to_xlsx_with_validations` 写多列 `dataValidations`/`formula1`；每列 255 字符 | `data_validation_lists_are_written_as_dropdowns` |
| 表头自动筛选 | 每个有数据的 sheet 输出 `<autoFilter ref="A1:最后一列最后行"/>`；可选关闭 | `conditional_rules_support_top_and_bottom_ranks` + `auto_filter_can_be_disabled` |
| 条件格式 dxf 去重 | 相同颜色规则复用同一 `dxfId`，`dxfs count` 为唯一颜色数 | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 日期周期 | `--highlight` 支持 `today/yesterday`、`last7/30Days`、`this/last/nextWeek`、`this/last/nextMonth`，导出 `timePeriod` | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 iconSet | 支持 `amount:iconSet`（默认 3TrafficLights1）或 value 选 3Symbols/3Arrows/3Arrows2/3Flags/3Stars；未知预设报错 | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 duplicate/unique | `--highlight` 支持 `amount:duplicate:brown`、`unique:navy`，导出 duplicateValues/uniqueValues | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 above/belowAverage | `--highlight` 支持 `amount:aboveAverage:teal`、`belowAverage`，导出 `AVERAGE(列)` 公式 | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 notContainsText | 支持 `amount:notContains:zz`（非空），导出 `ISERROR(SEARCH(...))`；引号/XML 均转义 | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 between | 支持 `between` / `notBetween`，恰好两边界；数值直出，文本加引号并转义 XML | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 blank/nonblank | `--highlight` 支持 `amount:blank:gray`、`nonblank:cyan`，导出 containsBlanks/notContainsBlanks | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 begins/ends | 支持 `beginsWith:12`、`endsWith:00`（非空），LEFT/RIGHT 公式，引号/XML 均转义（含 `&`） | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 dataBar | 支持 `amount:dataBar:#FF0000`，颜色必填且 #RRGGBB，导出 `dataBar` 最小/最大 | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 containsText | 支持 `amount:contains:00`（非空），导出 `containsText` + `SEARCH`；引号 `""`、XML `&amp;/&lt;/&gt;` 正确转义 | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 ColorScale | 支持 `scale2`（2 色）/`scale3`（3 色），颜色数匹配且均须 #RRGGBB，导出 `colorScale` + `percentile` 50 | `conditional_rules_support_top_and_bottom_ranks` |
| 条件格式 Top/Bottom | `--highlight` 支持 `top2`、`bottom1`、`top50%`、`bottom25%`，导出 `top10`/`bottom10`（可带 `percent`） | `conditional_rules_support_top_and_bottom_ranks` |
| 导入格式元数据 | `xlsx_column_formats` 读 header + 首数据行样式；`convert book.xlsx --formats [--json] [--all-sheets]` 打印列格式（JSON 含 sheet/column_index） | `column_formats_are_read_back_from_export_styles`、`formats_prints_the_xlsx_header_format_codes` |
| CLI 数字格式 | `--number-format "amount:#,##0.00;rate:0.0%"` 接线到导出样式 | `convert_xlsx_can_apply_per_column_number_formats` |
| 数字格式导出 | `write_sheets_with_formats` 把 Decimal/Percent/Currency/DateTime 映射为 numFmt + 单元格样式 | `formats_are_mapped_into_export_styles_and_round_trip` |
| 全 sheet 导出 | `--all-sheets` 每张 sheet 一个 CSV，含 hidden | `convert_extracts_every_xlsx_sheet_to_a_directory`、`convert_all_sheets_includes_hidden_ones` |
| sheet 名 | 大小写不敏感；默认跳过 hidden | `reads_a_named_sheet`、`hidden_sheets_are_not_selected_by_default` |
| 列上限 | >16384 拒绝 | `a_cell_beyond_excels_column_limit_is_an_error` |
| 行上限 | 解析中限制，表头单独计 | `sheet_rows_stops_before_an_unbounded_worksheet`、`table_from_rows_skips_empty_rows_and_enforces_the_limit` |
| inlineStr | 有 `<is>` 解析；空 body 视为空单元格 | xlsx fixture 与 `convert_imports_xlsx_shared_strings_and_sheets` |

## 8. 实测（2026-11-26，release）

```text
200,000 行 × 10 列，CSV 6.8 MB → xlsx 8.3 MB
导出 1.4–2.0 s；导回 CSV 0.93–0.96 s；round-trip 字节一致
复现：scripts/bench_xlsx_roundtrip.sh
```

1,000,000 行 × 10 列（CSV ≈34 MB，xlsx 43.5 MB）：
导出 7.18 s；导回 8.04 s；round-trip 字节一致；仍在 Excel 1,048,576 行上限内（含表头）。
导入峰值内存实测 53 MB（macOS `/usr/bin/time -l`）：worksheet XML 未压缩 402 MB，
改为 zip entry 逐块解压 + 行级解析 + 流式 CSV，不再整表载入；此前 1.02 GB。

同 200k 数据集在优化前导回需 240–295 s。原因是每个 cell/attribute 都重新编译正则；
改为线性标签扫描 + 手工属性解析 + `OnceLock` 缓存值正则后降到 0.93 s，差异约 250×。

`convert` 是用户可跑的入口；`read_xlsx_to_csv` 的错误会保留为 stderr，失败不写输出文件。
