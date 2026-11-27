# csvx 迭代进度

## 当前目标

继续 csvx 迭代：逐一校验 23 个 MCP 工具 schema 与 CLI 参数一致性；把 DSH 真实客户端验证做成可选脚本；继续加固/缩小 `crates/mcp`；保持 12 路由与 smoke 契约；每轮跑测试、全量 gate、AOCI、发布包，最多 push 一次。

## 已完成

- 删除 legacy MCP 协议循环，只保留官方 `rmcp`。
- `crates/mcp` 拆为 `lib.rs / json.rs / cache.rs / executor.rs / jobs.rs / rmcp_server.rs / tools.rs`，并新增 `executor/paths.rs`。
- 单文件行数上限进入 `check_layout.sh`。
- 官方 SDK 逐项调用全部 23 个工具，`sdk_calls=23`。
- DSH headless 作为真实 MCP 客户端验证成功。
- 修复 `csvx_edit_delete` 缺少 `count` 的真实 bug。
- `scripts/check_mcp_schema.py` 校验 23 工具、43 字段，`mcp_schema=ok`。
- `scripts/check_mcp_dsh.sh` 可选 DSH 回归，默认 `skip`。

## Excel 对齐滚动路线图（≥3 个未完成项）

- [x] 项1 公式函数覆盖审计与首批补齐：`docs/FORMULA_COVERAGE.md`；IFS/SWITCH/
  IFNA/CHOOSE/NA/XOR/TRUE/FALSE，IFERROR 捕获 #N/A；测试
  `logical_fallbacks_and_switch_are_lazy`。
- [x] 项2 数字格式/显示对齐：`docs/NUMBER_FORMAT_COVERAGE.md`；新增
  `yyyy-mm-dd`/`dd/mm/yyyy`/`hh:mm:ss` 等 8 种日期时间格式，测试
  `date_time_formats_render_iso_values`。
- [x] 项3 xlsx 导出数字格式保真：`write_sheets_with_formats`；Decimal/Percent/
  Currency/DateTime → numFmt + 样式；round-trip 测试。
- [x] 项4 条件显示规则增强：`top`/`bottom` rank 导出为 `top10`/`bottom10`；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项5 公式第二批：TEXT/TEXTBEFORE/TEXTAFTER/DATEVALUE/HOUR/MINUTE/SECOND；
  测试 `text_and_time_helpers_match_spreadsheet_expectations`。
- [x] 项6 数字格式第二批：`0.00E+00`、负数括号会计、`@` 文本；测试
  `scientific_accounting_and_text_formats_work`。
- [x] 项7 `--highlight` CLI 支持 `amount:top2:green` / `bottom1:red`，帮助与
  CLI 测试 `convert_xlsx_can_highlight_a_top_rank`。
- [x] 项8 convert 导出接线数字格式：`--number-format "amount:#,##0.00;rate:0.0%"`；
  CLI 测试 `convert_xlsx_can_apply_per_column_number_formats`。
- [x] 项9 xlsx 导入 numFmt → ColumnMeta 格式保真：`xlsx_column_formats` 读取
  header + 首数据行样式，返回 NumberFormat。
- [x] 项15 xlsx 导入科学/会计/@：同一 API 已覆盖 Scientific/Accounting/Text。
- [x] 项18 xlsx 导入日期 numFmt：DateTime 白名单读回；测试
  `column_formats_are_read_back_from_export_styles` 覆盖 6 列。
- [x] 项19 `xlsx_column_formats` 接到 CLI：`convert book.xlsx --formats` 打印
  `column<TAB>format`；CLI 测试 `formats_prints_the_xlsx_header_format_codes`。
- [x] 项20/33/34 公式第五批：RANK/QUARTILE/TRIMMEAN；测试
  `rank_orders_...`、`quartile_and_trimmean_...`。
- [x] 项21 日期时间格式白名单扩展：`m/d/yyyy`、`yyyy/m/d`；布局与 CLI 测试。
- [x] 项23 `--formats --json` 增加 sheet/column_index 元数据；CLI 测试扩展。
- [x] 项25 `--formats --sheet Hidden` 测试；`formats_can_target_a_hidden_sheet`。
- [x] 项26 `--formats` 非 xlsx/组合标志/`--json` 缺 formats 错误诊断；
  `formats_rejects_non_xlsx_and_conflicting_flags`。
- [x] 项27/28 `--formats --sheet Nope` 错误诊断 + JSON schema（sheet/columns/
  column_index）稳定测试；`formats_reports_a_missing_sheet_and_stable_json_shape`。
- [x] 项29 `--formats --all-sheets` 输出所有 sheet 的列格式（文本/JSON）；
  `formats_all_sheets_prints_each_sheet_and_json_array`。
- [x] 项30 内置日期 numFmt 14 读回 `yyyy-mm-dd`；
  `builtin_date_num_fmt_14_reads_as_an_iso_date`。
- [x] 项31/32 `--formats --all-sheets` 与 `--sheet` 冲突、空 workbook
  `no header row` 诊断；`formats_all_sheets_rejects_sheet_and_reports_an_empty_workbook`。
- [x] 项35 点号函数名（RANK.EQ/STDEV.S/VAR.S/STDEV.P/VAR.P）词法支持；
  测试 `dotted_statistical_aliases_parse_and_evaluate`。
- [x] 项36 PERCENTRANK（含 significance 截断）；测试扩展。
- [x] 项37 GEOMEAN/HARMEAN；测试 `geomean_and_harmmean_read_same_row_ranges`。
- [x] 项38 PERCENTILE.EXC / PERCENTILE.INC 别名；边界 0/1 报错。
- [x] 项34 公式 QUARTILE/TRIMMEAN（见上）。
- [x] 项22 `--formats --json` 机器可读输出；CLI 测试扩展。
- [x] 项17 DATEDIF 扩展单位 MD/YM/YD；测试 `datedif_supports_ym_md_and_yd`。
- [x] 项16 公式第四批：YEARFRAC/ISOWEEKNUM/TIME；测试
  `time_isoweeknum_and_yearfrac_work`。
- [x] 项10 数字格式诊断：重复列/非法码报错，`--number-format` 与 `--highlight`
  同用；CLI 测试 `number_format_rejects_repeated_columns_and_bad_codes`、
  `number_format_and_highlight_can_be_combined`。
- [x] 项11 公式第三批：TIMEVALUE/NOW/TODAY/DATEDIF；测试
  `timevalue_datedif_and_clock_cells_work`。
- [x] 项14 `--number-format` 日期时间格式导出；CLI 测试
  `number_format_exports_a_date_time_code`；修复列名/时间冒号拆分。
- [x] 项12 `TEXT()` 覆盖 Scientific/Accounting/@；公式测试扩展通过。
- [x] 项13 `--number-format` 导出 Scientific/Accounting/@ 样式；CLI 测试
  `number_format_exports_scientific_accounting_and_text_codes`；修复会计码
  `positive;(negative)` 被误拆列的问题。
- [x] 项129 边界加固合并（一次性收尾）：把项39–128 的周末/节假日/DAYS360
  微变体合并为「日期边界加固」，已由 `date_range_boundaries_work`、
  `workday_weekend_start_and_holiday_weekends_work` 等既有测试覆盖；后续同类
  微测试不再单独立项。
- [x] 项130 文本替换语义（功能级）：`SUBSTITUTE` 默认全替换、第 N 个
  `instance_num`、越界原样、<1 `#VALUE!`；`REPLACE` 改为 Excel 位置语义
  （start_num/num_chars，1-based，越界与非法参数报错）。测试
  `text_concatenation_and_slicing` 覆盖全部/第 N/越界/非法参数。
- [x] 项131 同行查找（功能级）：`INDEX(range, n)`、`INDEX(range,1,n)`、
  `MATCH(value, range, match_type)`、`XMATCH` 支持同行区间，1-based；越界
  `#VALUE!`，未命中 `#N/A`；MATCH 1/-1 与 XMATCH +1/-1 方向已区分。测试
  `index_match_and_xmatch_read_a_same_row_range`。
- [x] 项132 非 ASCII 码点（功能级）：`UNICHAR`/`UNICODE` 支持 Unicode 码点
  与字符互转；非法码点/空文本 `#VALUE!`；测试 `char_and_code` 覆盖中文、emoji、
  0/代理区/越界。
- [x] 项133 固定小数/货币文本（功能级）：`FIXED` 固定小数/千分位/
  `no_commas`；`DOLLAR` 货币与负数括号；`NUMBERVALUE` 区域分隔符、百分号、
  括号负数与非法分隔符报错。测试 `fixed_dollar_and_numbervalue_work`。
- [x] 项134 小计函数（功能级）：`SUBTOTAL(function_num, range)` 支持
  1–11 与 101–111；同行区间；空区间/非法编号 `#VALUE!`。测试
  `subtotal_supports_the_excel_function_numbers`、
  `subtotal_rejects_an_all_empty_range`。
- [x] 项141 类型信息（功能级）：`TYPE(value)` 返回数字=1/文本=2/布尔=4/
  错误=16；空单元格按 1。测试 `type_reports_values_and_errors`。
- [x] 项265 非 NA 错误判断（功能级）：`ISERR(value)`；
  除 `#N/A` 外错误返回 TRUE；测试 `iserr_ignores_na_but_sees_other_errors`。
- [x] 项266 包含式四分位（功能级）：`QUARTILE.INC(array, quart)`；
  非法 quart/空区间 `#VALUE!`；测试扩展 `quartile_and_trimmean_read_same_row_ranges`。
- [x] 项267 百分位排名（功能级）：`PERCENTRANK.INC`/`PERCENTRANK.EXC`；
  非法参数/空区间 `#VALUE!`；测试扩展 `percentrank_and_percentile_aliases_read_same_row_ranges`。
- [x] 项268 众数别名（功能级）：`MODE.SNGL(array)`；
  空区间/无众数 `#VALUE!`；测试扩展 `mode_picks_the_most_frequent_value_of_a_same_row_range`。
- [x] 项269 众数多值（功能级）：`MODE.MULT(array)`；
  仅支持首众数/无众数 `#VALUE!`；测试扩展 `mode_picks_the_most_frequent_value_of_a_same_row_range`。
- [x] 项270 数组转文本（功能级）：`ARRAYTOTEXT(array, [format])`；
  单行区间转逗号分隔文本；非法 format/空区间 `#VALUE!`；测试 `arraytotext_renders_same_row_range`。
- [x] 项271 卡方拟合检验（功能级）：`CHISQ.TEST(observed, expected)`；
  单行等长区间、df=n-1；非法参数 `#VALUE!`；测试 `chisq_test_returns_goodness_of_fit_pvalue`。
- [x] 项272 文本拆分（功能级）：`TEXTSPLIT(text, delimiter)`；
  返回首个分隔片段（标量引擎限制）；空分隔符/缺参 `#VALUE!`；测试 `textsplit_returns_first_segment`。
- [x] 项273 查找未命中回退（功能级）：`TEXTBEFORE`/`TEXTAFTER` 支持 `if_not_found`；
  未命中返回回退值；测试 `textbefore_after_support_if_not_found`。
- [x] 项274 xlsx 导出数字格式保真（功能级）：内置时间格式映射 + 时间单元格导出；
  round-trip 测试 `formats_are_mapped_into_export_styles_and_round_trip`。
- [x] 项275 xlsx 日期时间导出（功能级）：`YYYY-MM-DDTHH:MM:SS` 写成日期单元格带时间小数；
  测试 `datetime_cells_are_written_with_a_time_fraction`。
- [x] 项276 xlsx 文本格式保真（功能级）：`@` 导出为 numFmt 样式并写回 inlineStr；
  round-trip 读取仍为 Text；测试 `formats_are_mapped_into_export_styles_and_round_trip`。
- [x] 项277 xlsx 文本列数字保真（功能级）：`@` 列中 `001`/`123` 保持 inlineStr，不转数值；
  round-trip 测试 `formats_are_mapped_into_export_styles_and_round_trip`。
- [x] 项264 精确伽马对数（功能级）：`GAMMALN.PRECISE(x)`；
  非正/非有限 `#VALUE!`；测试扩展 `gammaln_matches_log_gamma`。
- [x] 项263 逻辑/非文本判断（功能级）：`ISLOGICAL`/`ISNONTEXT`；
  无参 `#VALUE!`；测试 `logical_and_nontext_tests_work`。
- [x] 项262 精确误差函数（功能级）：`ERF.PRECISE`/`ERFC.PRECISE`；
  非法参数 `#VALUE!`；测试 `precise_error_functions_work`。
- [x] 项261 引用判断（功能级）：`ISREF(value)`；
  区间/引用返回 TRUE，值/文本返回 FALSE；测试 `isref_recognizes_references_without_evaluating`。
- [x] 项279 xlsx 列格式回退扫描（功能级）：首个数据行无样式时向下扫描；
  测试 `column_formats_fall_back_when_the_first_data_row_has_no_styles`。
- [x] 项280 分数数字格式（功能级）：`# ?/?` / `?/?` 解析与显示；
  测试 `fraction_formats_render_mixed_and_simple_forms`。
- [x] 项281 xlsx 分数格式导出（功能级）：`# ?/?` 样式写入并按格式 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项282 两位数分数格式（功能级）：`# ??/??` 解析/显示；
  分母≤99；测试 `fraction_formats_render_mixed_and_simple_forms`。
- [x] 项283 百分比千分位格式（功能级）：`#,##0.0%` 解析/显示；
  测试 `number_formats_render_the_way_a_spreadsheet_does`。
- [x] 项284 百分比格式导出（功能级）：`#,##0.0%` 样式写入并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项285 百分比零位小数格式（功能级）：`#,##0%` 解析/显示；
  测试 `number_formats_render_the_way_a_spreadsheet_does`。
- [x] 项286 百分比零位小数导出（功能级）：`#,##0%` 样式写入并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项287 12 小时制时间格式（功能级）：`h:mm AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项288 12 小时制 xlsx 导出（功能级）：`h:mm AM/PM` 样式写入并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项289 仅小时 12 小时制（功能级）：`h AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项290 12 小时制带秒导出（功能级）：`h:mm:ss AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项291 仅小时 12 小时制导出（功能级）：`hh AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项292 两位年份日期格式（功能级）：`m/d/yy` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项293 两位年份日期导出（功能级）：`m/d/yy` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项294 两位年份日期时间（功能级）：`m/d/yy h:mm` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项295 两位年份日期时间导出（功能级）：`m/d/yy h:mm` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项296 两位年份日期时间带秒（功能级）：`m/d/yy h:mm:ss` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项297 短横线两位年份日期（功能级）：`yy-mm-dd` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项298 两位年份日期时间带秒导出（功能级）：`m/d/yy h:mm:ss` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项299 短横线两位年份导出（功能级）：`yy-mm-dd` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项300 短横线两位年份日期时间（功能级）：`yy-mm-dd h:mm` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项301 短横线两位年份日期时间导出（功能级）：`yy-mm-dd h:mm` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项302 短横线两位年份日期时间带秒（功能级）：`yy-mm-dd h:mm:ss` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项303 短横线两位年份日期时间带秒导出（功能级）：`yy-mm-dd h:mm:ss` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项304 两位年份日期 12 小时制（功能级）：`m/d/yy h:mm AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项305 两位年份日期 12 小时制导出（功能级）：`m/d/yy h:mm AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项306 两位年份日期 12 小时制带秒（功能级）：`m/d/yy h:mm:ss AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项307 两位年份日期 12 小时制带秒导出（功能级）：`m/d/yy h:mm:ss AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项308 短横线两位年份 12 小时制（功能级）：`yy-mm-dd h:mm AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项309 短横线两位年份 12 小时制导出（功能级）：`yy-mm-dd h:mm AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项310 短横线两位年份 12 小时制带秒（功能级）：`yy-mm-dd h:mm:ss AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项311 短横线两位年份 12 小时制带秒导出（功能级）：`yy-mm-dd h:mm:ss AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项312 四位年份日期 12 小时制（功能级）：`yyyy-mm-dd hh:mm AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项313 四位年份日期 12 小时制导出（功能级）：`yyyy-mm-dd hh:mm AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项314 四位年份日期 12 小时制带秒（功能级）：`yyyy-mm-dd hh:mm:ss AM/PM` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项315 四位年份日期 12 小时制带秒导出（功能级）：`yyyy-mm-dd hh:mm:ss AM/PM` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项316 日在前短日期（功能级）：`d/m/yyyy` 解析/显示；
  测试 `date_time_formats_render_iso_values`。
- [x] 项317 补零月日短日期（功能级）：`mm/dd/yy` 解析/显示；
  测试 `date_time_formats_render_iso_values`（2024-01-02 → 01/02/24）。
- [x] 项318 日在前短日期导出（功能级）：`d/m/yyyy` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项319 补零月日短日期导出（功能级）：`mm/dd/yy` 样式并 round-trip；
  测试 `formats_are_mapped_into_export_styles_and_round_trip` 与 `column_formats_are_read_back_from_export_styles`。
- [x] 项260 第二类贝塞尔 Y 函数（功能级）：`BESSELY(x, n)`；
  整数阶 0–20、x>0；非法 `#VALUE!`；测试 `bessely_returns_second_kind_values`。
- [x] 项278 xlsx 列格式去重（功能级）：多行重复格式只生成一个 numFmt，列数不重复；
  测试 `repeated_column_formats_are_deduplicated_across_rows`。
- [x] 项323 经过时间格式（功能级）：`[h]:mm:ss` 数值显示，超过 24 小时累计；
  测试 `date_time_formats_render_iso_values`（0.5/1.5/2.25）。
- [x] 项324 WEEKNUM 周起始扩展（功能级）：return_type 2 与 11–17 支持任意周起始；
  测试 `weeknum_return_types_control_week_start`。
- [x] 项325 经过时间格式导出（功能级）：`[h]:mm:ss` 样式；数值/36:00:00 写成序列 1.5，raw CSV round-trip；
  测试 `elapsed_formats_export_as_serial_and_round_trip_raw`。
- [x] 项326 TEXTBEFORE/TEXTAFTER 匹配模式（功能级）：match_mode 0/1 + match_end + if_not_found；
  测试 `textbefore_after_honor_match_mode_and_match_end`。
- [x] 项327 CLEAN 控制字符清理（功能级）：`CLEAN(text)` 去掉 ASCII 0–31，不裁空格；
  测试 `clean_strips_control_characters`。
- [x] 项328 VALUETOTEXT 文本化（功能级）：format 0/1；数字/布尔/错误转文本；
  测试 `valuetotext_converts_values_and_quotes_strict_text`。
- [x] 项329 TEXTSPLIT ignore_empty（功能级）：跳过空段返回首个非空片段；
  测试 `textsplit_can_skip_empty_segments`。
- [x] 项330 TEXTBEFORE/AFTER Unicode 大小写不敏感（功能级）：match_mode 1 支持非 ASCII；
  测试 `textbefore_after_honor_match_mode_and_match_end`。
- [x] 项331 TEXT 科学计数/会计格式（功能级）：TEXT 复用 NumberFormat 的 E+/会计格式；
  测试 `text_renders_scientific_and_accounting_formats`。
- [x] 项332 SEARCH Unicode 大小写不敏感（功能级）：非 ASCII 折叠且按字符定位；
  测试 `search_folds_unicode_case_and_keeps_positions`。
- [x] 项333 TEXTSPLIT 行分隔符（功能级）：行分隔符取首行再按列分隔；
  测试 `textsplit_can_skip_empty_segments`。
- [x] 项334 TEXTSPLIT pad_with（功能级）：ignore_empty 后全空返回 pad_with；
  测试 `textsplit_can_skip_empty_segments`。
- [x] 项335 FIND/SEARCH start_num 校验（功能级）：start_num 非整数/小于 1/非数值 → #VALUE!；
  测试 `search_folds_unicode_case_and_keeps_positions`。
- [x] 项336 TEXTBEFORE/AFTER instance_num 校验（功能级）：非整数/0/非数值 → #VALUE!；
  测试 `textbefore_after_honor_match_mode_and_match_end`。
- [x] 项337 REPT 次数校验（功能级）：非整数/负数/超 10000 → #VALUE!；
  测试 `rept_requires_integer_count_within_limit`。
- [x] 项338 SUBSTITUTE instance_num 校验（功能级）：非整数/<1/非数值 → #VALUE!；
  定向测试通过。
- [x] 项339 REPT 输出上限（功能级）：结果 >32767 字符 → #VALUE!；
  测试 `rept_requires_integer_count_within_limit`。
- [x] 项340 MID start_num 校验（功能级）：start_num<1/非整数/非数值、num_chars 负数/非整数 → #VALUE!；
  测试 `mid_validates_start_and_length`。
- [x] 项341 LEFT/RIGHT 长度校验（功能级）：负数/小数/非数值/多参 → #VALUE!；
  测试 `left_right_validate_length`。
- [x] 项342 REPLACE start/count 校验（功能级）：非整数/负数/非数值 → #VALUE!；
  定向测试通过。
- [x] 项343 PROPER 撇号/连字符分界（功能级）：非字母分界大写，数字也分界；
  测试 `the_everyday_functions_behave_like_a_spreadsheet`。
- [x] 项344 TRIM 内部空格折叠（功能级）：连续 ASCII 空格压成单个，制表符保留；
  测试 `the_everyday_functions_behave_like_a_spreadsheet`。
- [x] 项345 TEXT 正/负/零三段格式（功能级）：按符号选段、负段取绝对值、负段减号；
  测试 `text_renders_sectioned_formats`。
- [x] 项346 CHAR 范围校验（功能级）：仅 1–255，越界/非数值/多参 → #VALUE!；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项347 TEXT 第 4 段文本格式（功能级）：`@` 占位/字面量替换；
  测试 `text_renders_sectioned_formats`。
- [x] 项348 CODE 参数校验（功能级）：空文本/多参 → #VALUE!；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项349 NUMBERVALUE 参数校验（功能级）：空文本/非法分隔符 → #VALUE!；
  测试 `fixed_dollar_and_numbervalue_work`。
- [x] 项350 UNICODE 参数校验（功能级）：空文本/多参 → #VALUE!（实现已有，补测试锁定）；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项351 VALUE 文本数字解析扩展（功能级）：货币/千分位/百分号/括号负数；
  测试 `fixed_dollar_and_numbervalue_work`。
- [x] 项352 TEXTJOIN 参数校验（功能级）：ignore_empty 非 0/1/小数/非数值 → #VALUE!；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项353 SEARCH 通配符（功能级）：`*`/`?`/`~` 转义，Unicode 折叠；
  测试 `search_folds_unicode_case_and_keeps_positions`。
- [x] 项354 DOLLAR 小数位校验（功能级）：decimals 须整数 0–4；
  测试 `fixed_dollar_and_numbervalue_work`。
- [x] 项355 XLOOKUP 通配符匹配（功能级）：第 5 参 match_mode 2，整键匹配；
  测试 `wildcard_keys_match_entire_values` + `xlookup_preloads...`。
- [x] 项356 MATCH 通配符（功能级）：精确匹配支持 `*`/`?`/`~`；
  测试 `index_match_and_xmatch_read_a_same_row_range`。
- [x] 项357 VLOOKUP 通配符（功能级）：第 4 参 match_mode 0/2，整键匹配；
  测试 `vlookup_preloads_the_same_reference_as_ref`。
- [x] 项358 XMATCH 通配符（功能级）：match_mode 2；
  测试 `index_match_and_xmatch_read_a_same_row_range`。
- [x] 项359 HLOOKUP 通配符（功能级）：精确模式支持 `*`/`?`/`~`；
  测试 `hlookup_and_lookup_read_single_row_tables`。
- [x] 项360 MATCH 近似文本（功能级）：文本候选按字典序，大小写不敏感；
  测试 `index_match_and_xmatch_read_a_same_row_range`。
- [x] 项361 XLOOKUP match_mode 1 近似（功能级）：最小 key ≥ 目标；
  测试 `approximate_reference_picks_the_next_larger_key`。
- [x] 项362 LOOKUP 文本近似（功能级）：复用文本近似位置，已补测试锁定；
  测试 `hlookup_and_lookup_read_single_row_tables`。
- [x] 项363 VLOOKUP 近似模式（功能级）：match_mode 1 复用近似 key；
  测试 `vlookup_preloads_the_same_reference_as_ref`。
- [x] 项364 INDEX 位置校验（功能级）：位置须正整数，小数/0 → #VALUE!；
  测试 `index_match_and_xmatch_read_a_same_row_range`。
- [x] 项365 XLOOKUP match_mode -1（功能级）：最大 key ≤ 目标；
  测试 `approximate_reference_picks_the_next_larger_key` 扩展。
- [x] 项366 XMATCH search_mode -1（功能级）：反向查找，精确/通配符；
  测试 `index_match_and_xmatch_read_a_same_row_range`。
- [x] 项367 HLOOKUP 文本近似（功能级）：近似模式复用文本候选；
  测试 `hlookup_and_lookup_read_single_row_tables`。
- [x] 项368 VLOOKUP 负向近似（功能级）：match_mode -1；
  测试 `vlookup_preloads_the_same_reference_as_ref`。
- [x] 项369 DOLLARDE/DOLLARFR 分数校验（功能级）：分母正整数，0/负/小数/非数值报错；
  测试 `dollar_fraction_round_trips`。
- [x] 项370 ROMAN form 校验（功能级）：form 须整数 0–4，小数报错；
  测试 `roman_and_arabic_round_trip`。
- [x] 项371 BASE 进制范围校验（功能级）：进制 2–36、整数；越界/小数报错；
  测试 `decimal_and_base_convert_textual_bases`。
- [x] 项372 DECIMAL 非法字符（功能级）：非法字符/仅符号 → #VALUE!；
  测试 `decimal_and_base_convert_radices`。
- [x] 项373 UNICHAR 码点校验（功能级）：0/代理区/小数/越界 → #VALUE!；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项374 ARABIC 小写支持（功能级）：`from_roman` 统一大写折叠，测试已覆盖；
  测试 `roman_and_arabic_round_trip`。
- [x] 项375 CHAR 整数校验（功能级）：小数码点 → #VALUE!；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项376 TEXTJOIN 长度上限（功能级）：结果 ≤32767，超限 #VALUE!；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项377 VALUE 欧洲小数逗号（功能级）：`1.234,56` → 1234.56，`1,23` → 1.23；
  测试 `fixed_dollar_and_numbervalue_work`。
- [x] 项378 CONCAT 长度上限（功能级）：结果 ≤32767，超限 #VALUE!；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项379 PROPER Unicode 边界（功能级）：is_alphabetic 分界 + to_uppercase 扩展；
  测试 `the_everyday_functions_behave_like_a_spreadsheet`。
- [x] 项380 TRIM 全角空格（功能级）：U+3000/NBSP 折叠，制表符保留；
  测试 `the_everyday_functions_behave_like_a_spreadsheet`。
- [x] 项381 UPPER/LOWER Unicode 扩展（功能级）：ß→SS，已验证；
  测试 `the_everyday_functions_behave_like_a_spreadsheet`。
- [x] 项382 CLEAN C1 控制符（功能级）：C0/DEL/C1 去除；
  测试 `the_everyday_functions_behave_like_a_spreadsheet`。
- [x] 项383 条件格式前 N 项（功能级）：top/bottom 已支持，扩展百分位 `top50%`；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项384 条件格式色阶（功能级）：scale2/scale3 导出 colorScale；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项385 条件格式包含文本（功能级）：containsText + SEARCH 公式；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项386 条件格式数据条（功能级）：dataBar 导出；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项387 条件格式 beginsWith/endsWith（功能级）：LEFT/RIGHT 公式；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项388 条件格式空值规则（功能级）：containsBlanks/notContainsBlanks；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项389 条件格式 between 规则（功能级）：between/notBetween 双公式；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项390 条件格式 notContainsText（功能级）：ISERROR(SEARCH(...))；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项391 TEXTJOIN 全空输出（功能级）：ignore_empty=1 全空返回空串；
  测试 `textjoin_char_and_code_work_like_a_spreadsheet`。
- [x] 项392 条件格式 aboveAverage（功能级）：aboveAverage/belowAverage；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项393 条件格式 belowAverage（功能级）：与 aboveAverage 同分支；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项394 条件格式 duplicateValues（功能级）：duplicateValues/uniqueValues；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项395 条件格式 iconSet（功能级）：3TrafficLights1；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项396 条件格式 dateOccurring（功能级）：today/yesterday/last7Days/last30Days；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项397 条件格式 iconSet 预设（功能级）：3Symbols/3Arrows/3Flags/3Stars；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项398 条件格式 last7Days 语义（功能级）：近 7/30 天公式已验证；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项399 条件格式 thisWeek（功能级）：thisWeek/lastWeek/nextWeek；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项400 条件格式 thisMonth（功能级）：本月 timePeriod；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项401 条件格式 nextMonth（功能级）：nextMonth 公式已锁定；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项402 条件格式 lastMonth（功能级）：lastMonth 公式已锁定；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项403 条件格式 notBetween（功能级）：双公式导出；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项404 条件格式 bottom10 百分位（功能级）：bottom25% 已导出 percent；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项405 条件格式 between 边界校验（功能级）：缺/多边界报错；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项406 条件格式 containsText 空文本（功能级）：空/空白文本报错；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项407 条件格式 notContainsText 空文本（功能级）：空值报错（上轮守卫已覆盖）；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项408 条件格式 beginsWith 空文本（功能级）：前后缀空值报错；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项409 条件格式 dataBar 空颜色（功能级）：空/非 #RRGGBB 报错；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项410 条件格式 iconSet 未知预设（功能级）：非法预设报错；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项411 条件格式 colorScale 颜色数量（功能级）：数量不匹配报错；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项412 条件格式 colorScale 非法颜色（功能级）：非 #RRGGBB 报错；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项413 条件格式 containsText 引号转义（功能级）：`"` → `""`；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项414 条件格式 between 文本边界（功能级）：文本边界加引号导出；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项415 条件格式 notContainsText 引号转义（功能级）：同 literal 助手；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项416 条件格式 begins/ends 引号转义（功能级）：公式改用 formula_literal；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项417 条件格式公式 XML 转义（功能级）：&/< 文本转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项418 条件格式 containsText XML & 转义（功能级）：&/< 已转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项419 条件格式 containsText 右尖括号（功能级）：`>` 已转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项420 条件格式 beginsWith XML & 转义（功能级）：`A&B` 已转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项421 条件格式 endsWith XML & 转义（功能级）：`B&C` 已转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项422 条件格式 notContainsText XML & 转义（功能级）：`P&Q` 已转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项423 条件格式 between XML & 文本边界（功能级）：`&` 边界转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项424 条件格式 notBetween XML 文本边界（功能级）：`&` 边界转义；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项425 条件格式 dxf 样式去重（功能级）：同色规则复用 dxfId；dxfs count=唯一色；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项435 xlsx autoFilter（功能级）：单/多 sheet 输出 autoFilter；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项436 xlsx autoFilter 多列（功能级）：A1:B4 双列范围已测；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项426 条件格式 dxf 去重跨列（功能级）：id 列绿色复用 amount 列 dxfId=0；
  测试 `conditional_rules_support_top_and_bottom_ranks`。
- [x] 项437 RAND/RANDBETWEEN（功能级）：xorshift + 参数边界；
  测试 `rand_and_randbetween_stay_in_bounds`。
- [x] 项438 xlsx 冻结开关（功能级）：`csv_to_xlsx_without_freeze` 省略 pane；
  测试 `header_freeze_can_be_disabled`。
- [x] 项441 no-freeze 目录/多 sheet（功能级）：write_sheets_without_freeze + CLI 目录分支；
  测试 `multi_sheet_export_can_skip_header_freeze`。
- [x] 项439 RANDBETWEEN 大范围（功能级）：>2^53 报 #VALUE!；
  测试 `rand_and_randbetween_stay_in_bounds`。
- [x] 项440 冻结开关 CLI 接线（功能级）：convert `--no-freeze` 接线；
  测试 `every_command_documents_the_flags_it_parses` + core 单测。
- [x] 项442 xlsx autoFilter 开关（功能级）：csv_to_xlsx_without_auto_filter；
  测试 `auto_filter_can_be_disabled`。
- [x] 项443 RANDBETWEEN 单元格参数（功能级）：lo/hi 引用求值；
  测试 `rand_and_randbetween_stay_in_bounds`。
- [x] 项444 no-freeze + number-format 组合（功能级）：options 同时关闭 pane/filter 并保留格式；
  测试 `options_can_disable_freeze_and_filter_with_formats`。
- [x] 项445 autoFilter CLI 开关（功能级）：convert `--no-auto-filter`；
  测试 `every_command_documents_the_flags_it_parses` + `options_can_disable_freeze_and_filter_with_formats`。
- [ ] 项446 多 sheet options + 格式（功能级）：write_sheets_with_options 带 formats；
  验收：单测 + gate。
- [ ] 项447 no-auto-filter + highlight 组合（功能级）：CLI 同时生效；
  验收：单测 + gate。
- [ ] 项427 条件格式 dxf 跨规则复用（功能级）：同色多规则同 dxfId；
  验收：单测 + gate。
- [ ] 项428 条件格式 dxf 蓝色规则同 id 断言（功能级）：蓝色多规则同 dxfId；
  验收：单测 + gate。
- [x] 项429 斜杠日期格式导出 round-trip（功能级）：m/d/yyyy、mm/dd/yyyy 写入 numFmt；
  测试 `slash_date_formats_map_into_export_styles`。
- [x] 项432 斜杠日期 12 小时制（功能级）：m/d/yyyy、mm/dd/yyyy 12 小时；
  测试 `dotted_date_formats_match_excel_orders`。
- [x] 项433 斜杠日期时间导出 round-trip（功能级）：m/d/yyyy h:mm[:ss]、12 小时写 numFmt；
  测试 `slash_date_formats_map_into_export_styles`。
- [x] 项434 斜杠日期 h/hh 导出渲染（功能级）：布局 h/hh 与导出 code 已覆盖；
  测试 `dotted_date_formats_match_excel_orders`。
- [x] 项430 斜杠日期格式 h/hh 导出（功能级）：导出 numFmt 已覆盖；
  测试 `slash_date_formats_map_into_export_styles`。
- [x] 项431 斜杠日期格式 h:mm 支持（功能级）：m/d/yyyy、mm/dd/yyyy 带时间；
  测试 `dotted_date_formats_match_excel_orders`。
- [x] 项322 日期时间格式码语法矩阵（功能级）：点/破折号/12 小时/h-hh 补零/导出 round-trip；
  测试 `dotted_date_formats_match_excel_orders` + `dotted_and_dashed_formats_map_into_export_styles`。
- [x] 项259 超几何反函数（功能级）：`HYPGEOM.INV(n, K, N, alpha)`；
  非法参数 `#VALUE!`；测试 `hypgeom_inv_finds_cutoff`。
- [x] 项258 第二类贝塞尔函数（功能级）：`BESSELK(x, n)`；
  非法参数 `#VALUE!`；测试 `besselk_returns_second_kind_values`。
- [x] 项257 超几何分布（功能级）：`HYPGEOM.DIST(s, n, K, N, cumulative)`；
  非法参数 `#VALUE!`；测试 `hypgeom_dist_returns_mass_or_cumulative`。
- [x] 项256 修正贝塞尔 I 函数（功能级）：`BESSELI(x, n)`；
  非法参数 `#VALUE!`；测试 `besseli_returns_modified_values`。
- [x] 项255 贝塞尔 J 函数（功能级）：`BESSELJ(x, n)`；
  非法参数 `#VALUE!`；测试 `besselj_returns_first_kind_values`。
- [x] 项254 t 检验（功能级）：`T.TEST(array1, array2, tails, type)`；
  非法参数 `#VALUE!`；测试 `t_test_returns_pvalues`。
- [x] 项253 t 置信区间（功能级）：`CONFIDENCE.T(alpha, sd, n)`；
  非法参数 `#VALUE!`；测试 `confidence_t_returns_half_width`。
- [x] 项252 正态置信区间（功能级）：`CONFIDENCE.NORM(alpha, sd, n)`；
  非法参数 `#VALUE!`；测试 `confidence_norm_returns_half_width`。
- [x] 项251 F 检验（功能级）：`F.TEST(array1, array2)`；
  空区间/非法 `#VALUE!`；测试 `f_test_returns_two_tailed_pvalue`。
- [x] 项250 Z 检验（功能级）：`Z.TEST(array, x, [sigma])`；
  空区间/非法 `#VALUE!`；测试 `z_test_returns_one_tailed_probability`。
- [x] 项249 伽马反函数（功能级）：`GAMMA.INV(p, alpha, beta)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `gamma_inv_returns_quantiles`。
- [x] 项248 Beta 反函数（功能级）：`BETA.INV(p, alpha, beta, A, B)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `beta_inv_returns_quantiles`。
- [x] 项247 卡方右尾反函数（功能级）：`CHISQ.INV.RT(p, df)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `chisq_inv_right_tail_returns_quantiles`。
- [x] 项246 卡方反函数（功能级）：`CHISQ.INV(p, df)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `chisq_inv_returns_quantiles`。
- [x] 项245 F 分布左尾反函数（功能级）：`F.INV(p, d1, d2)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `f_inv_returns_left_tail_quantiles`。
- [x] 项244 F 分布右尾反函数（功能级）：`F.INV.RT(p, d1, d2)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `f_inv_right_tail_returns_quantiles`。
- [x] 项243 反双曲倒数（功能级）：`ASECH`/`ACSCH`/`ACOTH`；
  定义域外/非法 `#VALUE!`；测试 `inverse_reciprocal_hyperbolic_work`。
- [x] 项242 卡方右尾（功能级）：`CHISQ.DIST.RT(x, df)`；
  非法参数 `#VALUE!`；测试 `chisq_dist_right_tail_works`。
- [x] 项241 一般正态反函数（功能级）：`NORM.INV(p, mean, sd)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `norm_inv_returns_quantiles`。
- [x] 项240 t 双尾反函数（功能级）：`T.INV.2T(p, df)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `t_inv_two_tail_returns_positive_quantile`。
- [x] 项239 二项区间分布（功能级）：`BINOM.DIST.RANGE(n, p, s, t)`；
  非法参数 `#VALUE!`；测试 `binom_dist_range_sums_interval`。
- [x] 项238 t 分布反函数（功能级）：`T.INV(probability, df)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `t_inv_returns_quantiles`。
- [x] 项237 t 分布双尾（功能级）：`T.DIST.2T(x, df)`；
  非法参数 `#VALUE!`；测试 `t_dist_two_tail_works`。
- [x] 项236 二项反函数（功能级）：`BINOM.INV(n, p, alpha)`；
  非法参数 `#VALUE!`；测试 `binom_inv_finds_cutoff`。
- [x] 项235 t 分布右尾（功能级）：`T.DIST.RT(x, df)`；
  非法参数 `#VALUE!`；测试 `t_dist_right_tail_works`。
- [x] 项234 F 分布右尾（功能级）：`F.DIST.RT(x, d1, d2)`；
  非法参数 `#VALUE!`；测试 `f_dist_right_tail_works`。
- [x] 项233 对数正态反函数（功能级）：`LOGNORM.INV(p, mean, sd)`；
  概率(0,1)外/非法 `#VALUE!`；测试 `lognorm_inv_returns_quantiles`。
- [x] 项232 t 分布（功能级）：`T.DIST(x, df, cumulative)`；
  非法参数 `#VALUE!`；测试 `t_dist_returns_density_or_cumulative`。
- [x] 项231 F 分布（功能级）：`F.DIST(x, d1, d2, cumulative)`；
  非法参数 `#VALUE!`；测试 `f_dist_returns_density_or_cumulative`。
- [x] 项230 一般正态分布（功能级）：`NORM.DIST(x, mean, sd, cumulative)`；
  非法参数 `#VALUE!`；测试 `norm_dist_returns_density_or_cumulative`。
- [x] 项229 卡方分布（功能级）：`CHISQ.DIST(x, df, cumulative)`；
  非法参数 `#VALUE!`；测试 `chisq_dist_returns_density_or_cumulative`。
- [x] 项228 伽马分布（功能级）：`GAMMA.DIST(x, alpha, beta, cumulative)`；
  非法参数 `#VALUE!`；测试 `gamma_dist_returns_density_or_cumulative`。
- [x] 项227 贝塔分布（功能级）：`BETA.DIST(x, alpha, beta, cumulative)`；
  非法参数 `#VALUE!`；测试 `beta_dist_returns_density_or_cumulative`。
- [x] 项226 威布尔分布（功能级）：`WEIBULL.DIST(x, alpha, beta, cumulative)`；
  非法参数 `#VALUE!`；测试 `weibull_dist_returns_density_or_cumulative`。
- [x] 项225 对数正态分布（功能级）：`LOGNORM.DIST(x, mean, sd, cumulative)`；
  非法参数 `#VALUE!`；测试 `lognorm_dist_returns_density_or_cumulative`。
- [x] 项224 二项分布（功能级）：`BINOM.DIST(k, n, p, cumulative)`；
  非法参数 `#VALUE!`；测试 `binom_dist_returns_mass_or_cumulative`。
- [x] 项223 指数分布（功能级）：`EXPON.DIST(x, lambda, cumulative)`；
  非法参数 `#VALUE!`；测试 `expon_dist_returns_density_or_cumulative`。
- [x] 项222 泊松分布（功能级）：`POISSON.DIST(x, mean, cumulative)`；
  非法参数 `#VALUE!`；测试 `poisson_dist_returns_mass_or_cumulative`。
- [x] 项221 标准正态分布（功能级）：`NORM.S.DIST(z, cumulative)`；
  非数值 `#VALUE!`；测试 `norm_s_dist_returns_cumulative_or_density`。
- [x] 项220 标准正态密度（功能级）：`PHI(z)`；非数值
  `#VALUE!`；测试 `phi_returns_normal_density`。
- [x] 项219 标准正态分位（功能级）：`NORM.S.INV`；概率(0,1)外
  `#VALUE!`；测试 `normal_inverse_matches_standard_quantiles`。
- [x] 项218 值类型转换（功能级）：`N`/`T`；数字/文本/布尔语义；
  测试 `n_and_t_coerce_values`。
- [x] 项217 高斯误差（功能级）：`GAUSS(z)`；非数值
  `#VALUE!`；测试 `gauss_returns_normal_half_probability`。
- [x] 项216 正割余割（功能级）：`SEC`/`CSC`；零点/非数值
  `#VALUE!`；测试 `sec_and_csc_are_reciprocals`。
- [x] 项215 可重复排列（功能级）：`PERMUTATIONA(n, k)`；非负整数/
  越界 `#VALUE!`；测试 `permutationa_counts_repetition_permutations`。
- [x] 项214 平方根 π（功能级）：`SQRTPI(number)`；负数/非数值
  `#VALUE!`；测试 `sqrtpi_scales_by_pi`。
- [x] 项213 复数余切（功能级）：`IMCOT`；零点/非法复数
  `#VALUE!`；测试 `complex_cotangent_works`。
- [x] 项212 复数余割正割（功能级）：`IMCSC`/`IMSEC`；零点/非法复数
  `#VALUE!`；测试 `complex_cosecant_and_secant_work`。
- [x] 项211 复数三角（功能级）：`IMSIN`/`IMCOS`/`IMTAN`；非法复数
  `#VALUE!`；测试 `complex_trigonometric_functions_work`。
- [x] 项210 双曲倒数（功能级）：`CSCH`/`SECH`/`COTH`；零点/非数值
  `#VALUE!`；测试 `reciprocal_hyperbolic_functions_work`。
- [x] 项209 复数双曲（功能级）：`IMCOSH`/`IMSINH`；非法复数
  `#VALUE!`；测试 `complex_hyperbolic_functions_work`。
- [x] 项208 角度弧度（功能级）：`DEGREES`/`RADIANS`；非数值
  `#VALUE!`；测试 `degrees_and_radians_convert`。
- [x] 项207 ISO 天花板（功能级）：`ISO.CEILING(number[, significance])`；
  零 significance/非法参数 `#VALUE!`；测试 `iso_ceiling_rounds_up`。
- [x] 项206 反双曲函数（功能级）：`ASINH`/`ACOSH`/`ATANH`；定义域外
  `#VALUE!`；测试 `inverse_hyperbolic_functions_work`。
- [x] 项205 双曲函数（功能级）：`SINH`/`COSH`/`TANH`；非数值
  `#VALUE!`；测试 `hyperbolic_functions_work`。
- [x] 项204 余切函数（功能级）：`COT`/`ACOT`；零点/非数值
  `#VALUE!`；测试 `cot_and_acot_work`。
- [x] 项203 可重复组合（功能级）：`COMBINA(n, k)`；非负整数/越界
  `#VALUE!`；测试 `combina_counts_repetition_combinations`。
- [x] 项202 伽马函数（功能级）：`GAMMA(number)`；非正/非有限
  `#VALUE!`；测试 `gamma_matches_factorials`。
- [x] 项201 误差函数（功能级）：`ERF`/`ERFC`；数值近似与边界；
  测试 `erf_and_erfc_approximate_error_functions`。
- [x] 项200 阈值阶跃（功能级）：`GESTEP(number[, step])`；大于等于阈值
  返回 1；测试 `gestep_compares_threshold`。
- [x] 项199 单位阶跃（功能级）：`DELTA(number1[, number2])`；相等返回 1，
  否则 0；测试 `delta_compares_numbers`。
- [x] 项198 复数二进制对数（功能级）：`IMLOG2`；零/非法复数
  `#VALUE!`；测试 `complex_base_two_log_works`。
- [x] 项197 复数自然对数（功能级）：`IMLN`；零/非法复数
  `#VALUE!`；测试 `complex_natural_log_works`。
- [x] 项196 复数乘幂（功能级）：`IMPOWER(inumber, power)`；零的负幂
  `#VALUE!`；测试 `complex_power_works`。
- [x] 项195 复数平方根（功能级）：`IMSQRT`；主平方根/零复数；
  测试 `complex_square_root_work`。
- [x] 项194 复数指数对数（功能级）：`IMEXP`/`IMLOG10`；零复数 log
  `#VALUE!`；测试 `complex_exponential_and_log_work`。
- [x] 项193 复数极坐标（功能级）：`IMCONJUGATE`/`IMARGUMENT`；零复数
  argument `#VALUE!`；测试 `complex_conjugate_and_argument_work`。
- [x] 项192 复数乘除（功能级）：`IMPRODUCT`/`IMDIV` 两个复数文本参数；
  除零/非法复数 `#VALUE!`；测试 `complex_product_and_division_work`。
- [x] 项191 复数加减（功能级）：`IMSUM`/`IMSUB` 两个复数文本参数；
  非法复数 `#VALUE!`；测试 `complex_sum_and_difference_work`。
- [x] 项190 回归标准误（功能级）：`STEYX(known_y, known_x)`；同行等长、
  样本≥3、零方差 `#VALUE!`；测试 `steyx_measures_regression_error`。
- [x] 项189 对数伽马（功能级）：`GAMMALN(number)`；非正/非有限
  `#VALUE!`；测试 `gammaln_matches_log_gamma`。
- [x] 项188 复数基础（功能级）：`COMPLEX`/`IMREAL`/`IMAGINARY`/`IMABS`；
  非法后缀/参数 `#VALUE!`；测试 `complex_numbers_round_trip`。
- [x] 项187 多项式系数（功能级）：`MULTINOMIAL(values)`；同行非负整数、
  空区间/负数 `#VALUE!`；测试 `multinomial_counts_arrangements`。
- [x] 项186 双阶乘（功能级）：`FACTDOUBLE(number)`；负数/非整数/
  过大 `#VALUE!`；测试 `factdouble_computes_double_factorials`。
- [x] 项185 罗马数字（功能级）：`ROMAN(number[, form])`/`ARABIC(text)`；
  范围/非法字符 `#VALUE!`；测试 `roman_and_arabic_round_trip`。
- [x] 项184 URL 编码（功能级）：`ENCODEURL(text)` 按 UTF-8 百分号编码；
  测试 `encodeurl_percent_encodes_text`。
- [x] 项183 分数美元（功能级）：`DOLLARDE`/`DOLLARFR`；分数分母≤0/
  非整数 `#VALUE!`；测试 `dollar_fraction_round_trips`。
- [x] 项182 文本感知极值（功能级）：`MAXA`/`MINA` 把布尔/文本按 1/0
  计入；空样本 `#VALUE!`；测试 `maxa_and_mina_treat_text_as_zero`。
- [x] 项181 文本感知平均（功能级）：`AVERAGEA(values)` 把布尔/文本按
  1/0 计入；空样本 `#VALUE!`；测试 `averagea_treats_text_and_booleans_as_numbers`。
- [x] 项180 总体文本统计（功能级）：`STDEVPA`/`VARPA` 把布尔与文本按
  1/0 计入总体样本；空样本 `#VALUE!`；测试 `population_text_statistics_work`。
- [x] 项179 交叉乘积和（功能级）：`SUMXMY2`/`SUMX2MY2`/`SUMX2PY2`；
  同行等长 x/y、长度不等 `#VALUE!`；测试 `paired_sum_functions_work`。
- [x] 项178 文本感知统计（功能级）：`STDEVA`/`VARA` 把布尔与文本按
  1/0 计入样本；非法参数 `#VALUE!`；测试 `stdeva_and_vara_treat_text_as_zero`。
- [x] 项177 级数求和（功能级）：`SERIESSUM(x, n, m, coefficients)`；
  系数区间/非法参数 `#VALUE!`；测试 `seriessum_sums_powers`。
- [x] 项176 费雪变换（功能级）：`FISHER(x)`/`FISHERINV(y)`；x 越界
  `#VALUE!`；测试 `fisher_transform_round_trips`。
- [x] 项174 MATH 舍入（功能级）：`CEILING.MATH`/`FLOOR.MATH`；significance/
  mode 语义与零值；测试 `math_rounding_functions_work`。
- [x] 项175 修正内部收益率（功能级）：`MIRR(values, finance_rate,
  reinvest_rate)`；正负现金流/非法利率 `#VALUE!`；测试
  `mirr_discounts_and_reinvests_cash_flows`。
- [x] 项173 奇偶判断（功能级）：`ISEVEN(number)`/`ISODD(number)`；非整数/
  非数值 `#VALUE!`；测试 `iseven_and_isodd_work`。
- [x] 项172 百分位排名：审计确认 `PERCENTRANK.INC`/`PERCENTRANK.EXC`
  已实现（含既有测试），关闭，不重复开发。
- [x] 项171 平均排名：审计确认 `RANK.AVG` 原已实现（并列平均名次），
  关闭，不重复开发。
- [x] 项170 平均绝对偏差（功能级）：`AVEDEV(values)`；同行数值区间/
  空区间 `#VALUE!`；测试 `avedev_measures_mean_absolute_deviation`。
- [x] 项169 线性预测（功能级）：`FORECAST(x, known_y, known_x)`/
  `FORECAST.LINEAR(...)`；同行等长/零方差 `#VALUE!`；测试
  `forecast_predicts_from_a_line`。
- [x] 项168 协方差（功能级）：`COVAR(known_y, known_x)`/
  `COVARIANCE.P(...)`；同行等长、零方差 `#VALUE!`；测试 `covariance_functions_work`。
- [x] 项167 偏差平方和（功能级）：`DEVSQ(values)`；同行数值区间/空区间
  `#VALUE!`；测试 `devsq_sums_squared_deviations`。
- [x] 项166 相关系数（功能级）：`PEARSON(known_y, known_x)`/`RSQ(...)`；
  同行等长、零方差 `#VALUE!`；测试 `pearson_and_rsq_measure_correlation`。
- [x] 项165 线性回归（功能级）：`SLOPE(known_y, known_x)`/
  `INTERCEPT(known_y, known_x)`；同行等长区间、零方差 `#VALUE!`；测试
  `slope_and_intercept_fit_a_line`。
- [x] 项164 标准化（功能级）：`STANDARDIZE(x, mean, standard_dev)`；
  standard_dev≤0/非数值 `#VALUE!`；测试 `standardize_returns_z_scores`。
- [x] 项163 几何/调和平均（功能级）：`GEOMEAN`/`HARMEAN`；同行正数样本、
  非正数/空样本 `#VALUE!`；测试 `geometric_and_harmonic_means_work`。
- [x] 项162 偏度/峰度（功能级）：`SKEW(values)`/`KURT(values)`；同行样本、
  样本量不足/零方差 `#VALUE!`；测试
  `skew_and_kurtosis_measure_distribution_shape`。
- [x] 项161 投资期数（功能级）：`PDURATION(rate, pv, fv)`；rate≤-1/
  非正 pv/fv `#VALUE!`；测试 `pduration_reports_growth_periods`。
- [x] 项160 变动余额折旧（功能级）：`VDB(cost, salvage, life, start,
  end[, factor[, no_switch]])`；区间/因子非法 `#VALUE!`；测试
  `vdb_varies_declining_balance_depreciation`。
- [x] 项159 累计利息/本金（功能级）：`CUMIPMT(rate, nper, pv, start,
  end, type)`/`CUMPRINC(...)`；区间/type 越界 `#VALUE!`；测试
  `cumulative_interest_and_principal_work`。
- [x] 项158 多期复利（功能级）：`FVSCHEDULE(principal, schedule)`；
  schedule 同行区间/空区间、rate≤-1 `#VALUE!`；测试
  `fvschedule_compounds_a_rate_schedule`。
- [x] 项157 日期内部收益率（功能级）：`XIRR(values, dates[, guess])`；
  按天 Newton 求根；需正负现金流；非法日期/长度不等 `#VALUE!`；测试
  `xirr_solves_date_based_cash_flows`。
- [x] 项156 分期利息/本金（功能级）：`IPMT(rate, per, nper, pv[, fv[, type]])`/
  `PPMT(...)`；per 越界/type 非法 `#VALUE!`；测试
  `ipmt_and_ppmt_split_payments`。
- [x] 项155 等效增长率（功能级）：`RRI(nper, pv, fv)`；nper≤0/非正
  pv/fv `#VALUE!`；测试 `rri_reports_equivalent_growth_rate`。
- [x] 项154 日期净现值（功能级）：`XNPV(rate, values, dates)`；values/dates
  等长、按实际天数/365 折现、非法日期/利率 `#VALUE!`；测试
  `xnpv_discounts_by_calendar_dates`。
- [x] 项153 余额递减折旧（功能级）：`DB(cost, salvage, life, period[,
  month])`/`DDB(cost, salvage, life, period[, factor])`；测试
  `declining_balance_depreciation_works`。
- [x] 项152 折旧函数（功能级）：`SLN(cost, salvage, life)`/
  `SYD(cost, salvage, life, period)`；life≤0/period 越界/非整数 `#VALUE!`；测试
  `straight_line_and_sum_of_years_depreciation`。
- [x] 项151 利率换算（功能级）：`EFFECT(nominal, npery)`/`NOMINAL(effect,
  npery)`；npery 正整数、负利率/非整数 `#VALUE!`；测试
  `effect_and_nominal_convert_rates`。
- [x] 项150 内部收益率（功能级）：`IRR(values[, guess])`；周期现金流
  Newton 迭代；需正负现金流；不收敛 `#VALUE!`。测试
  `irr_solves_periodic_cash_flows`。
- [x] 项149 财务利率（功能级）：`RATE(nper, pmt, pv[, fv[, type[, guess]]])`
  Newton 迭代；nper≤0/guess≤-1/不收敛 `#VALUE!`；测试
  `rate_solves_the_annuity_equation`。
- [x] 项148 净现值（功能级）：`NPV(rate, value...)`；现金流从第 1 期
  折现；rate≤-1/非数值/空区间 `#VALUE!`。测试 `npv_discounts_a_cash_flow_series`。
- [x] 项147 财务期数（功能级）：`NPER(rate, pmt, pv[, fv[, type]])`；
  rate=0/type 0/1/rate≤-1 语义；测试 `nper_reports_the_number_of_periods`。
- [x] 项146 财务现值（功能级）：`PV(rate, nper, pmt[, fv[, type]])`；
  rate=0/type 0/1/rate≤-1 语义；测试 `pv_discounts_future_cash_flows`。
- [x] 项145 财务终值（功能级）：`FV(rate, nper, pmt[, pv[, type]])`；
  rate=0/type 0/1/rate≤-1 语义；测试 `fv_projects_future_value`。
- [x] 项144 财务付款（功能级）：`PMT(rate, nper, pv[, fv[, type]])`；
  rate=0/type 0/1/rate≤-1/nper=0 语义；测试 `pmt_calculates_loan_payments`。
- [x] 项143 错误类型（功能级）：`ERROR.TYPE` 对 `#NULL!`/`#DIV/0!`/
  `#VALUE!`/`#REF!`/`#NAME?`/`#NUM!`/`#N/A` 返回 1–7；非错误 `#N/A`。
  测试 `error_type_maps_excel_codes`。
- [x] 项142 进制转换（功能级）：`DECIMAL(text, radix)`/`BASE(number, radix[,
  min_length])`；2–36 进制、非法输入 `#VALUE!`；测试
  `decimal_and_base_convert_radices`。
- [x] 项140 国际工作日（功能级）：`NETWORKDAYS.INTL`/`WORKDAY.INTL`
  支持 weekend 代码 1–7/11–17、7 位 0/1 字符串与节假日；非法 weekend
  `#VALUE!`。测试 `intl_workday_functions_work`。
- [x] 项139 工程舍入（功能级）：`MROUND`（同号倍数，异号/零倍数
  `#VALUE!`）；`CEILING.PRECISE`/`FLOOR.PRECISE` 忽略 significance 符号。
  测试 `engineering_rounding_works`。
- [x] 项138 位运算函数（功能级）：`BITAND`/`BITOR`/`BITXOR` 支持 48 位
  非负整数；非法/非整数/超范围 `#VALUE!`。测试 `bitwise_functions_work`。
- [x] 项137 AGGREGATE（功能级）：`AGGREGATE(function_num, options, range[, k])`
  支持 1–11/14–19、options 0/4/6；k 用于排名/分位；非法编号/选项/缺少 k
  `#VALUE!`。测试 `aggregate_covers_subtotal_and_rank_families`。
- [x] 项136 数论/组合函数（功能级）：`GCD`/`LCM`/`FACT`/`COMBIN`/`PERMUT`
  支持非负整数；`FACT` 上限 170；非法/非整数 `#VALUE!`。测试
  `number_theory_functions_work`。
- [x] 项135 单行 HLOOKUP/LOOKUP（功能级）：HLOOKUP 同行单行表且
  `row_index=1`；LOOKUP 向量形式，结果区间等宽；精确/近似；越界/不等宽
  `#VALUE!`、未命中 `#N/A`。测试 `hlookup_and_lookup_read_single_row_tables`。

## 路线图质量门槛

- 每一项必须是功能级变更：新函数/新格式能力/新 CLI/API 能力/真实 bug 修复；
- 已支持函数的纯测试边界变体不再独立立项，归入「边界加固」并一次性收尾；
- 每轮最多收 1 个功能级项；未完成项恒 ≥3，来源只能是覆盖矩阵缺失或
  `BEYOND_EXCEL.md`/`CAPABILITIES.md` 缺口表。

## 当前进度

Round 465（项445/444 autoFilter CLI + 组合，功能级）：`--no-auto-filter`；
定向与全量 gate/package 通过；发布包 SHA `072bc8ae…ed0549d6`；
轮首 push 成功，本轮不再 push。

Round 464（项442 autoFilter 开关，功能级）：核心 API 可省略筛选；
clippy 首跑 too_many_arguments（已 allow）；全量 gate/package 通过；
发布包 SHA `43a5b496…01986a62`；轮首 push 成功，本轮不再 push。

Round 463（项443 RANDBETWEEN 单元格参数，功能级）；
定向与全量 gate/package 通过；发布包 SHA `4cef237a…b514d3c`；
轮首 push 成功，本轮不再 push。

Round 462（项439 RANDBETWEEN 大范围守卫，功能级）：>2^53 报错；
定向与全量 gate/package 通过；发布包 SHA `b518c2c7…c23b1084`；
轮首 push 成功，本轮不再 push。

Round 461（项441 no-freeze 多 sheet，功能级）；
定向与全量 gate/package 通过；发布包 SHA `4fd445f2…f00ce689`；
轮首 push 成功，本轮不再 push。

Round 460（项440 冻结开关 CLI，功能级）：convert `--no-freeze`；
定向与全量 gate/package 通过；发布包 SHA `74aad2ae…5089ed5571`；
轮首 push 成功，本轮不再 push。

Round 459（项438 xlsx 冻结开关，功能级）：核心 API 省略 pane；
定向与全量 gate/package 通过；发布包 SHA `566d4b98…06a94f5db`；
轮首 push 成功，本轮不再 push。

Round 458（项437 RAND/RANDBETWEEN，功能级）：xorshift 随机；
clippy 首跑因 helper 位于 test 模块之后失败（已前移）；docs 首跑因历史字样残留失败（已删）；
全量 gate/package 通过；发布包 SHA `1ee7246e…8f1b9a6`；轮首 push 成功，本轮不再 push。

Round 457（项426 dxf 跨列复用，功能级）：id 列绿色 dxfId=0；
定向与全量 gate/package 通过（docs 测试首跑因冻结开关字样失败，已改为文字）；
发布包 SHA `2ae5dca6…b257d82`；轮首 push 成功，本轮不再 push。

Round 456（项435/436 xlsx autoFilter，功能级）：A1:B4；
定向与全量 gate/package 通过；发布包 SHA `0bf061e0…ffc889e3`；
轮首 push 成功，本轮不再 push。

Round 455（项430/434 斜杠 h/hh 导出核验，功能级）；轮首 push 成功；
定向与全量 gate/package 通过；发布包 SHA `fd5614fa…78a34f25`；
本轮不再 push。

Round 454（项433 斜杠日期时间导出，功能级）：4 列 numFmt；
定向与全量 gate/package 通过；发布包 SHA `5fb8fcc5…86ecffe8`；
轮首 push 成功，本轮不再 push。

Round 453（项432 斜杠 12 小时制，功能级）：4 种格式；
定向与全量 gate/package 通过；发布包 SHA `7a97861f…7eeeec8a`；
轮首 push 成功，本轮不再 push。

Round 452（项431 斜杠日期时间，功能级）：m/d/yyyy h:mm 等 4 种；
定向与全量 gate/package 通过；发布包 SHA `79256697…82761fe`；
轮首 push 成功，本轮不再 push。

Round 451（项429 斜杠日期导出，功能级）：m/d/yyyy 等写入 numFmt；
定向与全量 gate/package 通过；发布包 SHA `c26242c1…c6da92769`；
轮首 push 成功，本轮不再 push。

Round 450（项322 收官：h/hh 补零，功能级）：9:05 vs 09:05；
定向与全量 gate/package 通过；发布包 SHA `78d1f369…9ebb2acf`；
轮首 push 成功，本轮不再 push。

Round 449（项322 日期格式矩阵第 4 片，功能级）：点/破折号导出样式 round-trip；
定向与全量 gate/package 通过；发布包 SHA `0dd6dd08…bdba0d39`；
轮首 push 成功，本轮不再 push。

Round 448（项322 日期格式矩阵第 3 片，功能级）：12 小时制 5 种；重试 push 成功补齐 2 提交；
定向与全量 gate/package 通过；发布包 SHA `6c6b46c6…5d015546`；
本轮不再 push。

Round 447（项322 日期格式矩阵第 2 片，功能级）：破折号 5 种；push 失败（本地 ahead=2）；
定向与全量 gate/package 通过；发布包 SHA `d2cad827…bb0178a9`。

Round 446（项322 日期格式矩阵第 1 片，功能级）：点分隔 8 种；
定向与全量 gate/package 通过；发布包 SHA `58e9ad33…47b7fab7`；
轮首 push 成功，本轮不再 push。

Round 445（项425 条件格式 dxf 去重，功能级）：唯一色 4；
定向与全量 gate/package 通过；发布包 SHA `1618df95…18bfb53f8`；
轮首 push 成功，本轮不再 push。

Round 444（项424 notBetween XML 文本边界，功能级）；
定向与全量 gate/package 通过；发布包 SHA `f86e5348…736233`；
轮首 push 成功，本轮不再 push。

Round 443（项423 between XML 文本边界，功能级）；
定向与全量 gate/package 通过；发布包 SHA `64e6bc02…42b985372`；
轮首 push 成功，本轮不再 push。

Round 442（项422 notContainsText XML 转义，功能级）；
定向与全量 gate/package 通过；发布包 SHA `3b61657a…47ed0b7d2`；
轮首 push 成功，本轮不再 push。

Round 441（项421 endsWith XML 转义，功能级）；
定向与全量 gate/package 通过；发布包 SHA `1c8bf915…c72e1fb`；
轮首 push 成功，本轮不再 push。

Round 440（项419/420 右尖括号 + beginsWith XML 转义，功能级）；
定向与全量 gate/package 通过；发布包 SHA `384578b2…c1b5e1f2`；
轮首 push 成功，本轮不再 push。

Round 439（项418 containsText XML & 转义，功能级）；
定向与全量 gate/package 通过；发布包 SHA `ec08679e…5b91afe3`；
轮首 push 成功，本轮不再 push。

Round 438（项417 条件格式公式 XML 转义，功能级）；
定向与全量 gate/package 通过；发布包 SHA `834ca4e2…d3c93e4aa5`；
轮首 push 成功，本轮不再 push。

Round 437（项416 begins/ends 引号转义，功能级）：顺带修复缺右括号；
定向与全量 gate/package 通过；发布包 SHA `df6abfb3…bd9a16c3`；
轮首 push 成功，本轮不再 push。

Round 436（项415 notContainsText 引号转义，功能级）；
定向与全量 gate/package 通过；发布包 SHA `fc6641cd…a6662aee`；
轮首 push 成功，本轮不再 push。

Round 435（项414 between 文本边界，功能级）：重试 push 成功补齐 2 提交；
定向与全量 gate/package 通过；发布包 SHA `e795e5f5…a4768014`；
本轮不再 push。

Round 434（项413 containsText 引号转义，功能级）；
push 失败（本地 ahead=1）；gate/package 通过；发布包 SHA `d229ad1c…43b932f09`。

Round 433（项412 条件格式 colorScale 非法颜色，功能级）；
定向与全量 gate/package 通过；发布包 SHA `c7cfce93…b7d444d2`；
轮首 push 成功，本轮不再 push。

Round 432（项411 条件格式 colorScale 颜色数量，功能级）；
定向与全量 gate/package 通过；发布包 SHA `9e8901f0…a0970b87`；
轮首 push 成功，本轮不再 push。

Round 431（项410 条件格式 iconSet 预设校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `cb72ec79…84d3c475`；
轮首 push 成功，本轮不再 push。

Round 430（项409 条件格式 dataBar 颜色校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `2b51e705…fa4201c9`；
轮首 push 成功，本轮不再 push。

Round 429（项407/408 条件格式文本守卫，功能级）；
定向与全量 gate/package 通过；发布包 SHA `78362537…c53a673a`；
轮首 push 成功，本轮不再 push。

Round 428（项406 条件格式 containsText 空文本，功能级）；
定向与全量 gate/package 通过；发布包 SHA `8074e031…3a8388a3`；
轮首 push 成功，本轮不再 push。

Round 427（项405 条件格式 between 边界校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `50a29877…66a47726`；
轮首 push 成功，本轮不再 push。

Round 426（项404 条件格式 bottom 百分位，功能级）；
定向与全量 gate/package 通过；发布包 SHA `39aaa459…b50c239b`；
轮首 push 成功，本轮不再 push。

Round 425（项403 条件格式 notBetween，功能级）；
定向与全量 gate/package 通过；发布包 SHA `44987c09…7b59a2cd`；
轮首 push 成功，本轮不再 push。

Round 424（项401/402 条件格式 last/nextMonth，功能级）；
定向与全量 gate/package 通过；发布包 SHA `a926fb9c…e3f0ee0`；
轮首 push 成功，本轮不再 push。

Round 423（项400 条件格式 thisMonth，功能级）：月周期；
定向与全量 gate/package 通过；发布包 SHA `8b0929e4…873b2180b`；
轮首 push 成功，本轮不再 push。

Round 422（项399 条件格式 thisWeek，功能级）：周周期；
定向与全量 gate/package 通过；发布包 SHA `433dc364…b9f2449a`；
轮首 push 成功，本轮不再 push。

Round 421（项398 条件格式 last7Days，功能级）：公式锁定；
定向与全量 gate/package 通过；发布包 SHA `db3f97e7…bf26b9bf`；
轮首 push 成功，本轮不再 push。

Round 420（项397 条件格式 iconSet 预设，功能级）；
定向与全量 gate/package 通过；发布包 SHA `7face1d6…8ba1c361`；
轮首 push 成功，本轮不再 push。

Round 419（项396 条件格式 dateOccurring，功能级）：timePeriod；
定向与全量 gate/package 通过；发布包 SHA `1531cc76…3fe0ebb2`；
轮首 push 成功，本轮不再 push。

Round 418（项395 条件格式 iconSet，功能级）：3TrafficLights1；
定向与全量 gate/package 通过；发布包 SHA `b639d96f…aa41b97`；
轮首 push 成功，本轮不再 push。

Round 417（项393/394 条件格式 belowAverage + duplicate，功能级）；
定向与全量 gate/package 通过；发布包 SHA `5d1a6868…141846c9`；
轮首 push 成功，本轮不再 push。

Round 416（项392 条件格式 aboveAverage，功能级）；
定向与全量 gate/package 通过；发布包 SHA `7d9c5f4f…25ec0593`；
轮首 push 成功，本轮不再 push。

Round 415（项391 TEXTJOIN 全空，功能级）：补测试锁定；
定向与全量 gate/package 通过；发布包 SHA `1a63972e…95562b0e`；
轮首 push 成功，本轮不再 push。

Round 414（项390 条件格式 notContainsText，功能级）；
全量 gate 首跑 cli 196/1，重跑通过；发布包 SHA `50961085…4ec3a2b`；
轮首 push 成功，本轮不再 push。

Round 413（项389 条件格式 between，功能级）：双公式；
定向与全量 gate/package 通过；发布包 SHA `07afe116…01b81800`；
轮首 push 成功，本轮不再 push。

Round 412（项388 条件格式空值，功能级）：blank/nonblank；
定向与全量 gate/package 通过；发布包 SHA `fda4529f…f375758f`；
轮首 push 成功，本轮不再 push。

Round 411（项387 条件格式前缀/后缀，功能级）：beginsWith/endsWith；
定向与全量 gate/package 通过；发布包 SHA `9a1af3c2…ce775fd5`；
轮首 push 成功，本轮不再 push。

Round 410（项386 条件格式数据条，功能级）：dataBar；
定向与全量 gate/package 通过；发布包 SHA `0eb1e288…5f20500b`；
轮首 push 成功，本轮不再 push。

Round 409（项385 条件格式包含文本，功能级）：containsText；
定向与全量 gate/package 通过；发布包 SHA `d355b115…1b1a10cb`；
轮首 push 成功，本轮不再 push。

Round 408（项384 条件格式色阶，功能级）：scale2/scale3；
定向与全量 gate/package 通过；发布包 SHA `594fda72…426360b8`；
轮首 push 成功，本轮不再 push。

Round 407（项383 条件格式前 N 项，功能级）：新增百分位；
定向与全量 gate/package 通过；发布包 SHA `3b3c6ed1…20ca6543`；
轮首 push 成功，本轮不再 push。

Round 406（项382 CLEAN C1，功能级）：C0/DEL/C1 去除；
定向与全量 gate/package 通过；发布包 SHA `2e6f98cb…72d5f829`；
轮首 push 成功，本轮不再 push。

Round 405（项381 UPPER/LOWER Unicode，功能级）；
定向与全量 gate/package 通过；发布包 SHA `965e72f6…1842873ad`；
轮首 push 成功，本轮不再 push。

Round 404（项380 TRIM 全角空格，功能级）；
定向与全量 gate/package 通过；发布包 SHA `364824c8…bdee2f22`；
轮首 push 成功，本轮不再 push。

Round 403（项379 PROPER Unicode，功能级）：补测试锁定；
定向与全量 gate/package 通过；发布包 SHA `526bbc95…470ec110`；
轮首 push 成功，本轮不再 push。

Round 402（项378 CONCAT 上限，功能级）：结果 ≤32767；
定向与全量 gate/package 通过；发布包 SHA `8d1a4f73…b1725e4a`；
轮首 push 成功，本轮不再 push。

Round 401（项377 VALUE 欧洲小数，功能级）：逗号小数识别；
定向与全量 gate/package 通过；发布包 SHA `8899a8f2…6fda7463`；
轮首 push 成功，本轮不再 push。

Round 400（项376 TEXTJOIN 上限，功能级）：结果 ≤32767；
定向与全量 gate/package 通过；发布包 SHA `d7bb4143…e397439f`；
轮首 push 成功，本轮不再 push。

Round 399（项375 CHAR 整数校验，功能级）：小数码点拒绝；
定向与全量 gate/package 通过；发布包 SHA `05ec525c…eb5ed8d`；
轮首 push 成功，本轮不再 push。

Round 398（项374 ARABIC 小写，功能级）：已实现，补文档与测试确认；
定向与全量 gate/package 通过；发布包 SHA `97d8729e…fdfe5d8e`；
轮首 push 成功，本轮不再 push。

Round 397（项373 UNICHAR 码点校验，功能级）：小数码点拒绝；
定向与全量 gate/package 通过；发布包 SHA `2019221f…f2a0c347`；
轮首 push 成功，本轮不再 push。

Round 396（项372 DECIMAL 校验，功能级）：非法字符/仅符号；
定向与全量 gate/package 通过；发布包 SHA `e3ac3255…73321926`；
轮首 push 成功，本轮不再 push。

Round 395（项371 BASE 进制校验，功能级）：补测试锁定 2–36；
定向与全量 gate/package 通过；发布包 SHA `f3fafbfd…70ced4e`；
轮首 push 成功，本轮不再 push。

Round 394（项370 ROMAN form 校验，功能级）：form 须整数；
定向与全量 gate/package 通过；发布包 SHA `f915baeb…db585ef9`；
轮首 push 成功，本轮不再 push。

Round 393（项369 DOLLARDE 分数校验，功能级）：补测试锁定；
定向与全量 gate/package 通过；发布包 SHA `5ffbde31…9d6891b2`；
轮首 push 成功，本轮不再 push。

Round 392（项368 VLOOKUP -1，功能级）：下一较小；
定向与全量 gate/package 通过；发布包 SHA `4aceaffc…0caa189b`；
轮首 push 成功，本轮不再 push。

Round 391（项367 HLOOKUP 文本近似，功能级）：补测试锁定；
定向与全量 gate/package 通过；发布包 SHA `932cd296…f75ed0bd`；
轮首 push 成功，本轮不再 push。

Round 390（项366 XMATCH 反向，功能级）：search_mode -1；
定向与全量 gate/package 通过；发布包 SHA `178ba465…6a36a2f`；
轮首 push 成功，本轮不再 push。

Round 389（项365 XLOOKUP -1，功能级）：下一较小；
定向与全量 gate/package 通过；发布包 SHA `0faeb0a8…dde110c1`；
轮首 push 成功，本轮不再 push。

Round 388（项364 INDEX 位置校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `62e37bef…b9140676`；
轮首 push 成功，本轮不再 push。

Round 387（项363 VLOOKUP 近似，功能级）：match_mode 1；
定向与全量 gate/package 通过；发布包 SHA `d7ffc145…89d1d35f`；
轮首 push 成功，本轮不再 push。

Round 386（项362 LOOKUP 文本近似，功能级）：补测试锁定；
定向与全量 gate/package 通过；发布包 SHA `edead006…66559b78`；
轮首 push 成功，本轮不再 push。

Round 385（项361 XLOOKUP 近似，功能级）：match_mode 1；
定向与全量 gate/package 通过；发布包 SHA `b965b391…1ba0044a`；
轮首 push 成功，本轮不再 push。

Round 384（项360 MATCH 近似文本，功能级）；
定向与全量 gate/package 通过（首跑 agent 9/1，重跑通过）；
发布包 SHA `4dc0f490…7c177ddd`；轮首 push 成功，本轮不再 push。

Round 383（项359 HLOOKUP 通配符，功能级）；
定向与全量 gate/package 通过；发布包 SHA `0496219b…12577a00`；
轮首 push 成功，本轮不再 push。

Round 382（项358 XMATCH 通配符，功能级）：match_mode 2；
定向与全量 gate/package 通过；发布包 SHA `6039ee31…cc4aeb5`；
轮首 push 失败，本轮不再 push。

Round 381（项357 VLOOKUP 通配符，功能级）：match_mode 2；
定向与全量 gate/package 通过；发布包 SHA `e28ee4dc…f8e43c0d`；
轮首 push 成功，本轮不再 push。

Round 380（项356 MATCH 通配符，功能级）；
定向与全量 gate/package 通过；发布包 SHA `991cfb57…0649bdfd`；
轮首 push 成功，本轮不再 push。

Round 379（项355 XLOOKUP 通配符，功能级）：match_mode 2；
定向与全量 gate/package 通过；发布包 SHA `d9fcd7a6…cc51893`；
轮首 push 成功，本轮不再 push。

Round 378（项354 DOLLAR 小数位，功能级）：0–4 整数；
定向与全量 gate/package 通过；发布包 SHA `38f72b0d…c3cea7b2`；
轮首 push 成功，本轮不再 push。

Round 377（项353 SEARCH 通配符，功能级）；
定向与全量 gate/package 通过；发布包 SHA `1db11615…565126d5`；
轮首 push 成功，本轮不再 push。

Round 376（项352 TEXTJOIN 校验，功能级）：ignore_empty 0/1；
定向与全量 gate/package 通过；发布包 SHA `bd384071…cc5ceaa3`；
轮首 push 成功，本轮不再 push。

Round 375（项351 VALUE 解析扩展，功能级）：货币/千分位；
定向与全量 gate/package 通过；发布包 SHA `c6923738…d4915d07`；
轮首 push 成功，本轮不再 push。

Round 374（项350 UNICODE 校验，功能级）：补测试锁定；
定向与全量 gate/package 通过；发布包 SHA `b3b45a77…43a70b0`；
轮首 push 成功，本轮不再 push。

Round 373（项349 NUMBERVALUE 校验，功能级）：空文本；
定向与全量 gate/package 通过；发布包 SHA `c21107a3…b3d4d669`；
轮首 push 成功，本轮不再 push。

Round 372（项348 CODE 参数校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `261bf6ba…39d3046e0`；
轮首 push 成功，本轮不再 push。

Round 371（项347 TEXT 第 4 段，功能级）：文本段；
定向与全量 gate/package 通过；发布包 SHA `e74fbe70…d259ce1a`；
轮首 push 成功，本轮不再 push。

Round 370（项346 CHAR 范围校验，功能级）：1–255；
定向与全量 gate/package 通过；发布包 SHA `3dd3171f…5a8047ce`；
轮首 push 成功，本轮不再 push。

Round 369（项345 TEXT 三段格式，功能级）：符号选段；
定向与全量 gate/package 通过；发布包 SHA `3245053b…287f0f63`；
轮首 push 成功，本轮不再 push。

Round 368（项344 TRIM 空格折叠，功能级）：内部连续空格；
定向与全量 gate/package 通过；发布包 SHA `6f487669…547fff5b9`；
轮首 push 成功，本轮不再 push。

Round 367（项343 PROPER 分界，功能级）：非字母分界；
定向与全量 gate/package 通过；发布包 SHA `1438d944…e11eaa4`；
轮首 push 成功，本轮不再 push。

Round 366（项342 REPLACE start/count 校验，功能级）：整数/非负；
定向与全量 gate/package 通过；发布包 SHA `64423485…c5f63b79`；
轮首 push 成功，本轮不再 push。

Round 365（项341 LEFT/RIGHT 长度校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `72c3c1c2…cc0ff75`；
轮首 push 成功，本轮不再 push。

Round 364（项340 MID start_num 校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `5849c636…73426e1`；
轮首 push 成功，本轮不再 push。

Round 363（项339 REPT 输出上限，功能级）：32767 字符；
定向与全量 gate/package 通过；发布包 SHA `84973113…108e8065`；
轮首 push 成功，本轮不再 push。

Round 362（项338 SUBSTITUTE instance_num 校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `98bf3d0a…d38c33a9`；
轮首 push 成功，本轮不再 push。

Round 361（项337 REPT 次数校验，功能级）：整数 0–10000；
定向与全量 gate/package 通过；发布包 SHA `1fecccac…3d1c9a1`；
轮首 push 成功，本轮不再 push。

Round 360（项336 TEXTBEFORE instance_num 校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `3bef6ea7…6a234e`；
轮首 push 成功，本轮不再 push。

Round 359（项335 FIND/SEARCH start_num 校验，功能级）；
定向与全量 gate/package 通过；发布包 SHA `87250161…429c0415`；
轮首 push 成功，本轮不再 push。

Round 358（项334 TEXTSPLIT pad_with，功能级）：全空段回退 pad；
定向与全量 gate/package 通过；发布包 SHA `f0def82c…94d0d8e`；
轮首 push 成功，本轮不再 push。

Round 357（项333 TEXTSPLIT 行分隔符，功能级）：首行首列语义；
定向与全量 gate/package 通过；发布包 SHA `deb09a17…b36f56ef`；
轮首 push 成功，本轮不再 push。

Round 356（项332 SEARCH Unicode，功能级）：Unicode 折叠、字符位置；
定向与全量 gate/package 通过；发布包 SHA `e279940d…0e8f7fa7`；
轮首 push 失败，本轮不再 push。

Round 355（项331 TEXT 科学计数/会计，功能级）：验证并锁定格式渲染；
定向与全量 gate/package 通过；发布包 SHA `997e8489…4ff205`；
轮首 push 成功，本轮不再 push。

Round 354（项330 Unicode 大小写不敏感，功能级）：TEXTBEFORE/AFTER match_mode 1；
定向与全量 gate/package 通过；发布包 SHA `baea9a10…44a0051c`；
轮首 push 失败，本轮不再 push。

Round 353（项329 TEXTSPLIT ignore_empty，功能级）：跳过空段；
定向与全量 gate/package 通过；发布包 SHA `5e9abb58…c702eb8c`；
轮首 push 成功，本轮不再 push。

Round 352（项328 VALUETOTEXT，功能级）：format 0/1 与错误文本化；
定向与全量 gate/package 通过；发布包 SHA `53199425…cb8e3fb44`；
轮首 push 成功，本轮不再 push。

Round 351（项327 CLEAN，功能级）：ASCII 控制字符清理；
定向与全量 gate/package 通过；发布包 SHA `848ec701…07cfc71c`；
轮首 push 成功，本轮不再 push。

Round 350（项326 TEXTBEFORE/AFTER 匹配模式，功能级）：match_mode/match_end/if_not_found；
定向与全量 gate/package 通过；发布包 SHA `1b4c0433…687d3865`；
轮首 push 成功，本轮不再 push。

Round 349（项325 经过时间导出，功能级）：`[h]:mm:ss` 序列 1.5、raw round-trip；
定向与全量 gate/package 通过；发布包 SHA `b987fc62…ec95aca1`；
轮首 push 成功，本轮不再 push。

Round 348（项324 WEEKNUM 周起始，功能级）：return_type 11–17 + 非法类型校验；
定向与全量 gate/package 通过；发布包 SHA `e4ce86db…1e37b913`；
轮首 push 成功，本轮不再 push。

Round 347（项323 经过时间格式，功能级）：`[h]:mm:ss` 数值显示，跨 24 小时累计；
定向与全量 gate/package 通过；发布包 SHA `c3f8e6a4…69097d9`；
轮首 push 成功，本轮不再 push。

Round 346（项278 列格式去重，功能级）：多行重复格式只写一个 numFmt；真实修复/验证项；
定向与全量 gate/package 通过；发布包 SHA `8d30229f…91542556`；
本轮未 push（累计提交待下轮一次推送）。

Round 345（项260 BESSELY，功能级）：整数阶 0–20、x>0；结构不同的新函数；
定向与全量 gate/package 通过；发布包 SHA `495e2c54…37be89`；
本轮未 push。按巡检纠偏：320/321 合并入项322 格式语法矩阵；
未完成项改为 278 去重（真实修复）、322 矩阵、323 经过时间（新能力）。

Round 344（项319 mm/dd/yy 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `833b4933…6df538e4`；
轮首 push 成功，本轮不再 push。

Round 343（项318 d/m/yyyy 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `bc5c3924…0d27daca`；
轮首 push 成功，本轮不再 push。

Round 342（项317 mm/dd/yy，功能级）：补零月日短日期；
定向与全量 gate/package 通过；发布包 SHA `1903271d…267181f0`；
轮首 push 成功，本轮不再 push。

Round 341（项316 d/m/yyyy，功能级）：日在前短日期；
定向与全量 gate/package 通过；发布包 SHA `1f6fe1f0…31bf97141`；
轮首 push 成功，本轮不再 push。

Round 340（项315 yyyy-mm-dd hh:mm:ss AM/PM 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `2319a6d4…90c5331`；
轮首 push 成功，本轮不再 push。

Round 339（项314 yyyy-mm-dd hh:mm:ss AM/PM，功能级）：四位年份 12 小时制带秒；
定向与全量 gate/package 通过；发布包 SHA `7c81c5a7…9de77f07`；
轮首 push 成功，本轮不再 push。

Round 338（项313 yyyy-mm-dd hh:mm AM/PM 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `224cbd64…f79789c0`；
轮首 push 成功，本轮不再 push。

Round 337（项312 yyyy-mm-dd hh:mm AM/PM，功能级）：四位年份 12 小时制；
定向与全量 gate/package 通过；发布包 SHA `b33978e0…72f351a7`；
轮首 push 成功，本轮不再 push。

Round 336（项311 yy-mm-dd h:mm:ss AM/PM 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `ae77d4e0…d15ecc00`；
轮首 push 成功，本轮不再 push。

Round 335（项310 yy-mm-dd h:mm:ss AM/PM，功能级）：短横线两位年份 12 小时制带秒；
定向与全量 gate/package 通过；发布包 SHA `e8b4a99d…177eca03a`；
轮首 push 成功，本轮不再 push。

Round 334（项309 yy-mm-dd h:mm AM/PM 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `65ae54ba…a76b817`；
轮首 push 成功，本轮不再 push。

Round 333（项308 yy-mm-dd h:mm AM/PM，功能级）：短横线两位年份 12 小时制；
定向与全量 gate/package 通过；发布包 SHA `1793db87…9ab60e9bc1`；
轮首 push 成功，本轮不再 push。

Round 332（项307 m/d/yy h:mm:ss AM/PM 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `dfa47e69…455da0594`；
轮首 push 成功，本轮不再 push。

Round 331（项306 m/d/yy h:mm:ss AM/PM，功能级）：两位年份 12 小时制带秒；
定向与全量 gate/package 通过；发布包 SHA `855e2e99…319d46e41`；
轮首 push 成功，本轮不再 push。

Round 330（项305 m/d/yy h:mm AM/PM 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `026edfaa…9adb5d7f`；
轮首 push 成功，本轮不再 push。

Round 329（项304 m/d/yy h:mm AM/PM，功能级）：两位年份 + 12 小时制；
定向与全量 gate/package 通过；发布包 SHA `80b3244b…b8557641`；
轮首 push 成功，本轮不再 push。

Round 328（项303 yy-mm-dd h:mm:ss 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `6d95a43d…800990b`；
轮首 push 成功，本轮不再 push。

Round 327（项302 yy-mm-dd h:mm:ss，功能级）：短横线两位年份带秒；
定向与全量 gate/package 通过；发布包 SHA `3ca35064…175e849b`；
轮首 push 成功，本轮不再 push。

Round 326（项301 yy-mm-dd h:mm 导出，功能级）：样式与 round-trip；
定向测试通过；首次 gate 在 agent 测试 9/1 失败，单测重跑 10/10 通过，gate 重跑通过；
发布包 SHA `f2c61913…6e7da6ae`；轮首 push 成功，本轮不再 push。

Round 325（项300 yy-mm-dd h:mm，功能级）：短横线两位年份日期时间解析/显示；
定向与全量 gate/package 通过；发布包 SHA `a32e78c7…cee680c4`；
轮首 push 成功（累积 2 提交已上云），本轮不再 push。

Round 324（项299 yy-mm-dd 导出，功能级）：短横线两位年份样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `e442deb7…0ed690a0`；
轮首 push 失败，本轮不再 push。

Round 323（项298 m/d/yy h:mm:ss 导出，功能级）：样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `d0f78d4e…c1147727`；
轮首 push 成功，本轮不再 push。

Round 322（项297 yy-mm-dd，功能级）：短横线两位年份解析/显示；
定向与全量 gate/package 通过；发布包 SHA `d02ca552…f5965cbe`；
轮首 push 成功，本轮不再 push。

Round 321（项296 两位年份日期时间带秒，功能级）：m/d/yy h:mm:ss 解析/显示；
定向与全量 gate/package 通过；发布包 SHA `386a3607…bc0f561d6`；
轮首 push 成功，本轮不再 push。

Round 320（项295 两位年份日期时间导出，功能级）：m/d/yy h:mm 样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `7fd5c9fe…1927ba31`；
轮首 push 成功，本轮不再 push。

Round 319（项294 两位年份日期时间，功能级）：m/d/yy h:mm 解析/显示；
定向与全量 gate/package 通过；发布包 SHA `0799b6cf…bcc48741`；
轮首 push 成功（累积 2 提交已上云），本轮不再 push。

Round 318（项293 两位年份日期导出，功能级）：m/d/yy 样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `e77c5953…0ef15cf20`；
轮首 push 失败，本轮不再 push。

Round 317（项292 两位年份日期，功能级）：m/d/yy 解析/显示；
定向与全量 gate/package 通过；发布包 SHA `1fd46dd4…c0a612c7`；
push 已在轮首成功，本轮不再 push。

Round 316（项291 hh AM/PM 导出，功能级）：仅小时 12 小时制样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `df1b7cd7…d3f88a74`；
push 已在轮首成功，本轮不再 push。

Round 315（项290 12 小时制带秒导出，功能级）：h:mm:ss AM/PM 样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `a5d7ff29…93504695`；
push 已在轮首成功，本轮不再 push。

Round 314（项289 仅小时 12 小时制，功能级）：h AM/PM / hh AM/PM；
定向与全量 gate/package 通过；发布包 SHA `7526cdcf…cbf9fc64`；
push 已在轮首成功，本轮不再 push。

Round 313（项288 12 小时制 xlsx 导出，功能级）：h:mm AM/PM 样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `2c696c4d…cdc3dd6a`；
push 已在轮首成功，本轮不再 push。

Round 312（项287 12 小时制时间，功能级）：h:mm AM/PM 解析/显示；
定向与全量 gate/package 通过；发布包 SHA `4b4152d9…83ac3cb2`；
push 已在轮首成功，本轮不再 push。

Round 311（项286 百分比零位小数导出，功能级）：#,##0% 样式与 round-trip；
定向与全量 gate/package 通过；发布包 SHA `dea34614…8d8de313`；
push 已在轮首成功，本轮不再 push。

Round 310（项285 百分比零位小数，功能级）：#,##0% 解析/显示；
定向与全量 gate/package 通过；发布包 SHA `be171eb6…0f092096`；
push 已在轮首成功，本轮不再 push。

Round 309（项284 百分比 xlsx 导出，功能级）：#,##0.0% 样式与列格式 round-trip；
定向与全量 gate/package 通过；发布包 SHA `a68acef8…e664984e`；
push 已在轮首成功，本轮不再 push。

Round 308（项283 百分比千分位，功能级）：#,##0.0% 解析/显示；
定向与全量 gate/package 通过；发布包 SHA `88a687f4…122187c9`；
push 已在轮首成功，本轮不再 push。

Round 307（项282 两位数分数，功能级）：# ??/?? 解析与显示；
定向与全量 gate/package 通过；发布包 SHA `8d0e393c…dcbc81ba`；
push 已在轮首成功，本轮不再 push。

Round 306（项281 xlsx 分数格式导出，功能级）：分数样式写入与列格式 round-trip；
定向与全量 gate/package 通过；发布包 SHA `bbcde84d…569f45e`；
push 已在轮首成功，本轮不再 push。

Round 305（项280 分数数字格式，功能级）：混合/简单分数解析与显示；
定向与全量 gate/package 通过；发布包 SHA `19789fd7…96bdd53c0`；
push 已在轮首成功，本轮不再 push。

Round 304（项279 xlsx 列格式回退扫描，功能级）：首行无样式向下扫描；
定向与全量 gate/package 通过；发布包 SHA `7b4a16b9…8cb827`；
push 已在轮首成功，本轮不再 push。

Round 303（项264 GAMMALN.PRECISE，功能级）：精确伽马对数别名、非正域；
定向与全量 gate/package 通过；发布包 SHA `e57f1c7f…3032dd55`；
push 已在轮首成功，本轮不再 push。

Round 302（项277 xlsx 文本列数字保真，功能级）：001/123 inlineStr、round-trip；
定向与全量 gate/package 通过；发布包 SHA `38ceb363…8604ad0f2`；
push 已在轮首成功，本轮不再 push。

Round 301（项276 xlsx 文本格式 @，功能级）：inlineStr + 样式、round-trip；
定向与全量 gate/package 通过；发布包 SHA `bfecb276…e71330bd`；
push 已在轮首成功，本轮不再 push。

Round 300（项275 xlsx 日期时间导出，功能级）：ISO 日期时间小数、非法保持；
定向与全量 gate/package 通过；发布包 SHA `dababf78…86980410`；
push 已在轮首成功，本轮不再 push。

Round 299（项274 xlsx 数字格式保真，功能级）：内置时间格式映射、时间单元格导出；
定向与全量 gate/package 通过；发布包 SHA `094decc7…07d6f16`；
push 已在轮首成功，本轮不再 push。

Round 298（项273 TEXTBEFORE/TEXTAFTER if_not_found，功能级）：未命中回退、参数边界；
定向与全量 gate/package 通过；发布包 SHA `82b7db4e…b9e30d67`；
push 已在轮首成功，本轮不再 push。

Round 297（项272 TEXTSPLIT，功能级）：首分隔片段、空分隔符；
定向与全量 gate/package 通过；发布包 SHA `bd1e34d2…4a492e38`；
push 已在轮首成功，本轮不再 push。

Round 296（项271 CHISQ.TEST，功能级）：单行拟合检验、df=n-1；
定向与全量 gate/package 通过；发布包 SHA `a333b5c3…1805cdc3`；
push 已在轮首成功，本轮不再 push。

Round 295（项270 ARRAYTOTEXT，功能级）：单行区间文本、格式 0/1；
定向与全量 gate/package 通过；发布包 SHA `215b171b…3739a2e25`；
push 已在轮首成功，本轮不再 push；AOCI 仍 Not connected。

Round 294（项269 MODE.MULT，功能级）：首众数、无众数；
定向与全量 gate/package 通过；发布包 SHA `b0d2115e…0aa32c1f`；
push 已在轮首成功，本轮不再 push；AOCI 仍 Not connected。

Round 293（项268 MODE.SNGL，功能级）：众数别名、并列/无众数；
定向与全量 gate/package 通过；发布包 SHA `06b29f12…13af212`；
push 已在轮首成功，本轮不再 push；AOCI 服务 Not connected，待下轮重试。

Round 292（项267 PERCENTRANK.INC/EXC，功能级）：百分位排名、significance；
定向与全量 gate/package 通过；发布包 SHA `9c284848…2499531d`；
push 已在轮首成功，本轮不再 push。

Round 291（项266 QUARTILE.INC，功能级）：包含式四分位别名；
定向与全量 gate/package 通过；发布包 SHA `d383a9f7…5e6650b5`；
push 已在轮首成功，本轮不再 push。

Round 290（项265 ISERR，功能级）：错误分类、排除 N/A；
定向与全量 gate/package 通过；发布包 SHA `47fc3659…dfa9bd78`；
push 已在轮首成功，本轮不再 push。

Round 289（项263 ISLOGICAL/ISNONTEXT，功能级）：布尔/文本分类、N/A 处理；
定向与全量 gate/package 通过；发布包 SHA `ebc8e3b9…e80791f0f`；
push 已在轮首成功，本轮不再 push。

Round 288（项262 ERF.PRECISE/ERFC.PRECISE，功能级）：精确误差函数；
定向与全量 gate/package 通过；发布包 SHA `11da4f89…47cd207f`；
push 已在轮首失败，本轮保留本地。

Round 287（项261 ISREF，功能级）：表达式形状判断、不求值；
定向与全量 gate/package 通过；发布包 SHA `71652362…105b761762`；
push 已在轮首成功，本轮不再 push。

Round 286（项259 HYPGEOM.INV，功能级）：超几何分位、alpha 边界；
定向与全量 gate/package 通过；发布包 SHA `cf96d200…80f70709`；
push 已在轮首成功，本轮不再 push。

Round 285（项258 BESSELK，功能级）：整数阶第二类贝塞尔、数值积分；
定向与全量 gate 通过；package 首轮 phase 阈值 flake，重跑通过；
发布包 SHA `b44b4ec4…115f3ded`；push 已在轮首成功，本轮不再 push。

Round 284（项257 HYPGEOM.DIST，功能级）：超几何质量/累积、边界；
定向与全量 gate/package 通过；发布包 SHA `61d17e04…d0d502cb`；
push 已在轮首成功，本轮不再 push。

Round 283（项256 BESSELI，功能级）：整数阶修正贝塞尔、级数；
定向与全量 gate/package 通过；发布包 SHA `1ff3f9da…f957dc2b`；
push 已在轮首成功，本轮不再 push。

Round 282（项255 BESSELJ，功能级）：整数阶第一类贝塞尔、级数；
定向与全量 gate/package 通过；发布包 SHA `51b10586…d24209c6`；
push 已在轮首成功，本轮不再 push。

Round 281（项254 T.TEST，功能级）：成对/等方差/Welch、单双尾；
定向与全量 gate/package 通过；发布包 SHA `8317b9f7…56254181`；
push 已在轮首成功，本轮不再 push。

Round 280（项253 CONFIDENCE.T，功能级）：t 置信半宽、参数边界；
定向测试通过；首轮 gate 遇 serve 测试 flake，重跑通过；package SHA
`40d2d099…4bb2bcf4`；push 已在轮首成功，本轮不再 push。

Round 279（项252 CONFIDENCE.NORM，功能级）：正态置信半宽、参数边界；
定向与全量 gate/package 通过；发布包 SHA `76b6af17…7fe1ee`；
push 已在轮首成功，本轮不再 push。

Round 278（项251 F.TEST，功能级）：双尾方差齐性、单行区间；
定向与全量 gate/package 通过；发布包 SHA `983d44e9…de17aad5`；
轮首已 push 两个待推提交，本轮不再 push。

Round 277（项250 Z.TEST，功能级）：单行区间、样本/已知 sigma；
定向与全量 gate/package 通过；发布包 SHA `acee6918…f34fc370`；
push 已在轮首成功，本轮不再 push。

Round 276（项249 GAMMA.INV，功能级）：伽马分位、参数边界；
定向与全量 gate/package 通过；发布包 SHA `c6af61fe…5ea08a78`；
push 已在轮首成功，本轮不再 push。

Round 275（项248 BETA.INV，功能级）：Beta 分位、可选上下界；
定向与全量 gate/package 通过；发布包 SHA `620a0d49…779a2a7d`；
push 已在轮首成功，本轮不再 push。

Round 274（项247 CHISQ.INV.RT，功能级）：卡方右尾分位、参数边界；
定向与全量 gate/package 通过；发布包 SHA `a20c9497…50d2c580`；
push 已在轮首成功，本轮不再 push。

Round 273（项246 CHISQ.INV，功能级）：卡方分位、参数边界；
定向与全量 gate/package 通过；发布包 SHA `0a18aa16…6879cfea`；
push 已在轮首成功，本轮不再 push。

Round 272（项245 F.INV，功能级）：F 左尾反函数、严格概率边界；
定向与全量 gate/package 通过；发布包 SHA `ae5c3455…ea4d5d20`；
push 已在轮首成功，本轮不再 push。

Round 271（项244 F.INV.RT，功能级）：F 右尾反函数、二分求解；
定向与全量 gate/package 通过；发布包 SHA `6a219320…70f196a`；
push 已在轮首成功，本轮不再 push。

Round 270（项243 ASECH/ACSCH/ACOTH，功能级）：反双曲倒数、定义域；
定向与全量 gate/package 通过；发布包 SHA `976bca6f…9f95406d`；
push 已在轮首成功，本轮不再 push。

Round 269（项242 CHISQ.DIST.RT，功能级）：卡方右尾、参数边界；
定向与全量 gate/package 通过；发布包 SHA `489d70f1…b6159d3b`；
push 已在轮首成功，本轮不再 push。

Round 268（项241 NORM.INV，功能级）：一般正态分位、参数边界；
定向与全量 gate/package 通过；发布包 SHA `a3da6180…b45a726c`；
push 已在轮首成功，本轮不再 push。

Round 267（项240 T.INV.2T，功能级）：双尾正分位、二分求解；
定向与全量 gate/package 通过；发布包 SHA `1247111d…8008caa`；
push 已在轮首失败，本轮保留本地。

Round 266（项239 BINOM.DIST.RANGE，功能级）：区间概率和、边界；
定向与全量 gate/package 通过；发布包 SHA `9f129f52…ed698a20`；
push 已在轮首成功，本轮不再 push。

Round 265（项238 T.INV，功能级）：t 分位、二分求解；
定向测试通过；首轮 gate 遇 cli 测试 flake，重跑通过；package SHA
`d9f87edc…f2b39b8b`；push 已在轮首成功，本轮不再 push。

Round 264（项237 T.DIST.2T，功能级）：t 双尾、参数边界；
定向与全量 gate/package 通过；发布包 SHA `7dd95f68…f16fba1f`；
push 已在轮首成功，本轮不再 push。

Round 263（项236 BINOM.INV，功能级）：累积反查、参数边界；
定向与全量 gate/package 通过；发布包 SHA `e0c8a263…7445ae7`；
push 已在轮首成功，本轮不再 push。

Round 262（项235 T.DIST.RT，功能级）：t 右尾、参数边界；
定向与全量 gate/package 通过；发布包 SHA `e6cea182…752a1856`；
push 已在轮首成功，本轮不再 push。

Round 261（项234 F.DIST.RT，功能级）：F 右尾、参数边界；
定向与全量 gate/package 通过；发布包 SHA `822d6e5a…f6b14426f`；
push 已在轮首成功，本轮不再 push。

Round 260（项233 LOGNORM.INV，功能级）：对数正态反函数、概率边界；
goal 已扩额至 1024 轮并 resume；定向与全量 gate/package 通过；
发布包 SHA `811f0f27…d7d66405`；push 已在轮首成功，本轮不再 push。

Round 259（项232 T.DIST，功能级）：t 密度/左尾；
定向与全量 gate/package 通过；发布包 SHA `e2565740…b6aafe`；
push 已在轮首成功，本轮不再 push。

Round 258（项231 F.DIST，功能级）：F 密度/累积；
定向与全量 gate/package 通过；发布包 SHA `d2e7e7fb…28033f68`；
push 已在轮首成功，本轮不再 push。

Round 257（项230 NORM.DIST，功能级）：任意正态密度/累积；
定向与全量 gate/package 通过；发布包 SHA `94a3645f…bdb430827`；
push 已在轮首成功，本轮不再 push。

Round 256（项229 CHISQ.DIST，功能级）：卡方密度/累积；
定向与全量 gate/package 通过；发布包 SHA `6750a6b0…ce1e029e`；
push 已在轮首成功，本轮不再 push。

Round 255（项228 GAMMA.DIST，功能级）：伽马密度/累积、正则化下不完全伽马；
定向与全量 gate/package 通过；发布包 SHA `3b555c31…9ea16fa7`；
push 已在轮首成功，本轮不再 push。

Round 254（项227 BETA.DIST，功能级）：正则化不完全贝塔、密度/累积；
定向与全量 gate/package 通过；发布包 SHA `7653d182…8120cea`；
push 已在轮首成功，本轮不再 push。

Round 253（项226 WEIBULL.DIST，功能级）：威布尔密度/累积；
定向与全量 gate/package 通过；发布包 SHA `22126606…14aa65b`；
push 已在轮首成功，本轮不再 push。

Round 252（项225 LOGNORM.DIST，功能级）：对数正态密度/累积；
定向与全量 gate/package 通过；发布包 SHA `413a55f8…215c5b1c`；
push 已在轮首成功，本轮不再 push。

Round 251（项224 BINOM.DIST，功能级）：二项质量/累积、参数边界；
定向与全量 gate/package 通过；发布包 SHA `4f7f906f…dc2e08f`；
push 已在轮首成功，本轮不再 push。

Round 250（项223 EXPON.DIST，功能级）：指数密度/累积、参数边界；
定向与全量 gate/package 通过；发布包 SHA `347b686d…3fe473c45`；
push 已在轮首成功，本轮不再 push。

Round 249（项222 POISSON.DIST，功能级）：泊松质量/累积、参数边界；
定向测试通过；首轮 gate 遇 1 个 cli 测试 flake，重跑通过；
package SHA `0cbbeca0…16976`；push 已在轮首成功，本轮不再 push。

Round 248（项221 NORM.S.DIST，功能级）：累积/密度双模式；
定向与全量 gate/package 通过；发布包 SHA `78c0b2f6…041507a`；
push 已在轮首成功，本轮不再 push。

Round 247（项220 PHI，功能级）：标准正态密度；
定向与全量 gate/package 通过；发布包 SHA `3dc72535…876726`；
push 已在轮首成功，本轮不再 push。

Round 246（项219 NORM.S.INV，功能级）：标准正态分位、概率边界；
TEXTBEFORE 审计发现已支持（受限），改用本项；定向测试通过；
全量 gate/package 通过；发布包 SHA `4d90181b…83f6e759`；
push 已在轮首成功，本轮不再 push。

Round 245（项218 N/T，功能级）：值类型强制转换、错误透传；
定向与全量 gate/package 通过；发布包 SHA `022d4d39…12dd65b`；
push 已在轮首成功，本轮不再 push。

Round 244（项217 GAUSS，功能级）：标准正态减半概率；
定向与全量 gate/package 通过；发布包 SHA `ff0986a1…f2b61020`；
push 已在轮首成功，本轮不再 push。

Round 243（项216 SEC/CSC，功能级）：正割/余割、零点；
定向与全量 gate/package 通过；发布包 SHA `ff00776a…170599e2`；
push 已在轮首成功，本轮不再 push。

Round 242（项215 PERMUTATIONA，功能级）：可重复排列、非负整数；
定向与全量 gate/package 通过；发布包 SHA `95827ba3…f98bdb70`；
push 已在轮首成功，本轮不再 push。

Round 241（项214 SQRTPI，功能级）：π 缩放平方根、负数边界；
定向与全量 gate/package 通过；发布包 SHA `798f1ab9…30fbdf`；
push 已在轮首成功，本轮不再 push。

Round 240（项213 IMCOT，功能级）：复数余切、零点；
定向与全量 gate/package 通过；发布包 SHA `793a43e7…e3b16910`；
push 已在轮首成功，本轮不再 push。

Round 239（项212 IMCSC/IMSEC，功能级）：复数余割/正割、零点；
定向与全量 gate/package 通过；发布包 SHA `73e6eee9…bb88c48c`；
push 已在轮首失败，本轮保留本地。

Round 238（项211 IMSIN/IMCOS/IMTAN，功能级）：复数三角；
定向与全量 gate/package 通过；发布包 SHA `3387b0c8…f534351`；
push 已在轮首成功，本轮不再 push。

Round 237（项210 CSCH/SECH/COTH，功能级）：双曲倒数、零点；
定向与全量 gate/package 通过；发布包 SHA `9fcf352b…e617814`；
push 已在轮首成功，本轮不再 push。

Round 236（项209 IMCOSH/IMSINH，功能级）：复数双曲函数；
定向与全量 gate/package 通过；发布包 SHA `defb006f…0b087a9`；
push 已在轮首成功，本轮不再 push。

Round 235（项208 DEGREES/RADIANS，功能级）：角度弧度互转；
定向与全量 gate/package 通过；发布包 SHA `d669e911…43ba149`；
push 已在轮首成功，本轮不再 push。

Round 234（项207 ISO.CEILING，功能级）：朝 +∞ 取整、绝对 significance；
定向与全量 gate/package 通过；发布包 SHA `d662a758…f4e46e5`；
push 已在轮首成功，本轮不再 push。

Round 233（项206 ASINH/ACOSH/ATANH，功能级）：反双曲函数与定义域；
定向与全量 gate/package 通过；发布包 SHA `e22e2ee0…5dc50c9`；
push 已在轮首成功，本轮不再 push。

Round 232（项205 SINH/COSH/TANH，功能级）：双曲函数与符号；
定向与全量 gate/package 通过；发布包 SHA `72b2b593…be846b`；
push 已在轮首成功，本轮不再 push。

Round 231（项204 COT/ACOT，功能级）：余切/反余切、零点与非数值；
定向与全量 gate/package 通过；发布包 SHA `a821537c…d6338aa`；
push 已在轮首成功，本轮不再 push。

Round 230（项203 COMBINA，功能级）：可重复组合、非负整数与数值边界；
定向与全量 gate/package 通过；发布包 SHA `c6e3c68b…66909`；
push 已在轮首成功，本轮不再 push。

Round 229（项202 GAMMA，功能级）：伽马函数、正数域与阶乘一致性；
定向与全量 gate/package 通过；发布包 SHA `c8560b6a…cfa3fb9140`；
push 已在轮首成功，本轮不再 push。

Round 228（项201 ERF/ERFC，功能级）：误差函数近似、上下限；
定向与全量 gate/package 通过；发布包 SHA `bfb4e4cc…0a7f6c35`；
push 已在轮首成功，本轮不再 push。

Round 227（项200 GESTEP，功能级）：阈值比较、默认 step；
定向与全量 gate/package 通过；发布包 SHA `3057c125…3825a947`；
push 已在轮首成功，本轮不再 push。

Round 226（项199 DELTA，功能级）：相等判断、默认第二参数；
定向与全量 gate/package 通过；发布包 SHA `9ae4684f…36b166`；
push 已在轮首成功，本轮不再 push。

Round 225（项198 IMLOG2，功能级）：二进制对数、零复数边界；
定向与全量 gate/package 通过；发布包 SHA `9fcb2796…b50a39cd9`；
push 已在轮首成功，本轮不再 push。

Round 224（项197 IMLN，功能级）：自然对数、零复数边界；
定向与全量 gate/package 通过；发布包 SHA `03792494…e0bfa1`；
push 已在轮首失败，本轮保留本地。

Round 223（项196 IMPOWER，功能级）：实幂、零的负幂与浮点归一；
定向与全量 gate/package 通过；发布包 SHA `437d3135…54ad422`；
push 已在轮首成功，本轮不再 push。

Round 222（项195 IMSQRT，功能级）：主平方根、零复数与负实数；
定向与全量 gate/package 通过；发布包 SHA `b2dc6069…f934b8f`；
push 已在轮首成功，本轮不再 push。

Round 221（项194 IMEXP/IMLOG10，功能级）：复数指数与对数、零复数边界；
定向与全量 gate/package 通过；发布包 SHA `cb4009c4…f8ddd1`；
push 已在轮首失败，本轮保留本地。

Round 220（项193 IMCONJUGATE/IMARGUMENT，功能级）：共轭、极角与零复数；
定向与全量 gate/package 通过；发布包 SHA `34fb8adc…89f5aa83`。

Round 219（项192 IMPRODUCT/IMDIV，功能级）：复数乘除、除零与后缀；
定向与全量 gate/package 通过；发布包 SHA `7427eea4…8af5d4d4`。

Round 218（项191 IMSUM/IMSUB，功能级）：复数加减、符号解析与后缀；
同时修复 macOS CI：`install_selfcheck.sh` 去掉 eval+here-doc，改为独立
python3 -c 读取 manifest；定向/全量 gate 通过；发布包 SHA `c2fe594c…bec592a`；push 已用一次。

Round 217（项190 STEYX，功能级）：回归标准误、样本量/零方差；
定向与全量 gate/package 通过；发布包 SHA `045b2827…15fe75`；
push 已在轮首成功，本轮不再 push。

Round 216（项189 GAMMALN，功能级）：Lanczos 对数伽马、正数域；
定向与全量 gate/package 通过；发布包 SHA `cda413c5…0cfb8003`；
push 已在轮首成功，本轮不再 push。

Round 215（项188 COMPLEX/IMREAL/IMAGINARY/IMABS，功能级）：复数构造与
访问器、i/j 后缀；定向与全量 gate/package 通过；发布包 SHA `1d78d0b4…275069c`；
push 已在轮首成功，本轮不再 push。

Round 214（项187 MULTINOMIAL，功能级）：多项式系数、整数/总和边界；
定向与全量 gate/package 通过；发布包 SHA `137feb27…53b37b`；
push 已在轮首成功，本轮不再 push。

Round 213（项186 FACTDOUBLE，功能级）：双阶乘、0/1 与范围边界；
定向与全量 gate/package 通过；发布包 SHA `dc70509a…fb2654`；
push 已在轮首成功，本轮不再 push。

Round 212（项185 ROMAN/ARABIC，功能级）：罗马数字双向、范围与 form；
定向与全量 gate/package 通过；发布包 SHA `486e3303…c490a57d`；
push 已在轮首成功，本轮不再 push。

Round 211（项184 ENCODEURL，功能级）：UTF-8 百分号编码、保留字符；
定向与全量 gate/package 通过；发布包 SHA `76fe7d30…a8ce21`；
push 已在轮首成功，本轮不再 push。

Round 210（项183 DOLLARDE/DOLLARFR，功能级）：分数美元双向转换、
分母边界；定向与全量 gate/package 通过；发布包 SHA `748e5310…3b56eb0`；
push 已在轮首成功，本轮不再 push。

Round 209（项182 MAXA/MINA，功能级）：文本/布尔极值、空样本；
定向与全量 gate/package 通过；发布包 SHA `42aa3087…3c3dd0`；
push 已在轮首成功，本轮不再 push。

Round 208（项181 AVERAGEA，功能级）：文本/布尔按 1/0、空样本；
定向与全量 gate/package 通过；发布包 SHA `22d51004…930fd7`；
push 已在轮首成功，本轮不再 push。

Round 207（项180 STDEVPA/VARPA，功能级）：总体文本统计、单值/空样本；
定向与全量 gate/package 通过；发布包 SHA `05f78630…1de27d`；
上轮 push 失败，本轮保留本地。

Round 206（项179 SUMXMY2/SUMX2MY2/SUMX2PY2，功能级）：成对区间聚合、
长度边界；定向与全量 gate/package 通过；发布包 SHA `98dbdcf1…2e8f6d`。

Round 205（项178 STDEVA/VARA，功能级）：文本/布尔按 1/0、样本量边界；
定向与全量 gate/package 通过；发布包 SHA `7e7b2101…82d21c`。

Round 204（项177 SERIESSUM，功能级）：级数幂次和、系数区间/空区间；
定向与全量 gate/package 通过；发布包 SHA `d2f35e5d…58622f`。

Round 203（项176 FISHER/FISHERINV，功能级）：Fisher z 变换与反变换、
定义域；定向与全量 gate/package 通过；发布包 SHA `e9b80510…36362c`。

Round 202（项175 MIRR，功能级）：正负现金流、finance/reinvest 分离与边界；
定向与全量 gate/package 通过；发布包 SHA `62e146c4…5ff4a7`。

Round 201（项174 CEILING.MATH/FLOOR.MATH，功能级）：significance/mode
方向、零 significance；定向与全量 gate/package 通过；发布包 SHA `8de3c2c9…b0c979`。

Round 200（项173 ISEVEN/ISODD，功能级）：项171/172 审计关闭（原已实现）；
新增奇偶判断；新增 MATH 舍入/MIRR/费雪变换待办；定向与全量 gate/package 通过；发布包 SHA `e4537e61…3a662d`。

Round 199（项170 AVEDEV，功能级）：平均绝对偏差、单值/空区间；
定向与全量 gate/package 通过；发布包 SHA `8913dc71…d68905`。

Round 198（项169 FORECAST，功能级）：线性预测、等长与零方差；
定向与全量 gate/package 通过；发布包 SHA `50ba9e57…89d729`。

Round 197（项168 COVAR/COVARIANCE.P，功能级）：协方差 P/S、等长与
样本量；定向与全量 gate/package 通过；发布包 SHA `8e09ad38…53988f`。

Round 196（项167 DEVSQ，功能级）：偏差平方和、单值/空区间；
定向与全量 gate/package 通过（首次 gate 波动，重跑通过）；发布包
SHA `59560ba8…eb1c3`。

Round 195（项166 PEARSON/RSQ，功能级）：相关系数/决定系数、正负相关与
零方差；定向与全量 gate/package 通过；发布包 SHA `662fed8d…bc08a`。

Round 194（项165 SLOPE/INTERCEPT，功能级）：最小二乘斜率/截距、等长与零方差；
定向与全量 gate/package 通过；发布包 SHA `4109e110…171320`。

Round 193（项164 STANDARDIZE，功能级）：z-score、标准差边界；
定向与全量 gate/package 通过；发布包 SHA `44ab1606…c1c3d`。

Round 192（项163 GEOMEAN/HARMEAN，功能级）：几何/调和平均、正数样本与
别名；定向与全量 gate/package 通过；发布包 SHA `93548d92…e7510e`。

Round 191（项162 SKEW/KURT，功能级）：偏度/峰度、样本量与零方差；
定向与全量 gate/package 通过；发布包 SHA `c8ecb898…cc038`。

Round 190（项161 PDURATION，功能级）：增长期数、零利率/非正边界；
定向与全量 gate/package 通过；发布包 SHA `7903f0bf…dcb2a`。

Round 189（项160 VDB，功能级）：变动余额折旧、区间/因子/开关边界；
定向与全量 gate/package 通过；发布包 SHA `bbc13b74…65e2a`。

Round 188（项159 CUMIPMT/CUMPRINC，功能级）：区间累计利息/本金、type/区间
边界；定向与全量 gate/package 通过；发布包 SHA `491e7958…f989b2`。

Round 187（项158 FVSCHEDULE，功能级）：多期复利、空区间/非法利率；
定向与全量 gate/package 通过；发布包 SHA `af912064…31206`。

Round 186（项157 XIRR，功能级）：按天现金流 Newton 求根、正负/日期校验；
定向与全量 gate/package 通过；发布包 SHA `9c423e7d…c0107`。

Round 185（项156 IPMT/PPMT，功能级）：利息/本金拆分、type/期数边界；
定向与全量 gate/package 通过；发布包 SHA `3bb66345…f51b9`。

Round 184（项155 RRI，功能级）：等效增长率、nper/pv/异号边界；
定向与全量 gate/package 通过；发布包 SHA `6bc713bc…f0909e`。

Round 183（项154 XNPV，功能级）：按天折现、values/dates 等长与非法日期；
定向与全量 gate/package 通过；发布包 SHA `9d74542b…96247`。

Round 182（项153 DB/DDB，功能级）：固定/双倍余额递减、period/月数/因子；
定向与全量 gate/package 通过；发布包 SHA `4f8e7db8…cba5b`。

Round 181（项152 SLN/SYD，功能级）：直线/年数总和折旧、life/period 边界；
定向与全量 gate/package 通过；发布包 SHA `078e1dae…9aef85`。

Round 180（项151 EFFECT/NOMINAL，功能级）：复利/名义利率换算、周期边界；
定向与全量 gate/package 通过；发布包 SHA `2f1a89a7…c5502`。

Round 179（项150 IRR，功能级）：周期现金流 Newton 求根、正负现金流校验；
定向与全量 gate/package 通过；发布包 SHA `bfb9583d…d961e3`。

Round 178（项149 RATE，功能级）：Newton 求根、guess/type 边界与不收敛；
定向与全量 gate/package 通过；发布包 SHA `992d5ffa…1519e`。

Round 177（项148 NPV，功能级）：现金流折现、空区间/非法利率；
定向与全量 gate/package 通过；发布包 SHA `52ec8cda…8a36f4`。

Round 176（项147 NPER，功能级）：rate=0/type 0/1、期数求解与非法参数；
定向与全量 gate/package 通过；发布包 SHA `baa4edb3…3ba870`。

Round 175（项146 PV，功能级）：rate=0/type 0/1、折现与非法参数；
定向与全量 gate/package 通过；发布包 SHA `4f4c83d0…547bd`。

Round 174（项145 FV，功能级）：rate=0/type 0/1、终值与非法参数；
定向与全量 gate/package 通过；发布包 SHA `b95b5365…158ae`。

Round 173（项144 PMT，功能级）：rate=0/type 0/1、rate≤-1/nper=0；
定向与全量 gate/package 通过；发布包 SHA `071322d0…fc15`。

Round 172（项143 ERROR.TYPE，功能级）：七类错误码映射与非错误 #N/A；
定向与全量 gate/package 通过；发布包 SHA `965024b0…fdbf8`。

Round 171（项142 进制转换，功能级）：DECIMAL/BASE、2–36 进制与错误边界；
定向与全量 gate/package 通过；发布包 SHA `34976a19…be259`。

Round 170（项141 TYPE，功能级）：数字/文本/布尔/错误类型码；定向与全量 gate/package 通过；发布包
SHA `d23c9f44…70fb5`。

Round 169（项140 国际工作日，功能级）：weekend 代码/字符串、节假日；
定向与全量 gate/package 通过；发布包 SHA `ded77353…a9848`。

Round 168（项139 工程舍入，功能级）：MROUND/PRECISE；同号、负倍数、
零倍数边界；定向与全量 gate/package 通过；发布包 SHA `9f186bec…01f52`。

Round 167（项138 位运算，功能级）：BITAND/BITOR/BITXOR；48 位边界与
非法参数；定向与全量 gate/package 通过；发布包 SHA `1bdf7653…b06df`。

Round 166（项137 AGGREGATE，功能级）：1–11/14–19、options 0/4/6、k 分位；
定向与全量 gate/package 通过；发布包 SHA `bf9367af…cfd06`。

Round 165（项136 数论/组合函数，功能级）：GCD/LCM/FACT/COMBIN/PERMUT；
非负整数域与错误语义；定向与全量 gate/package 通过；发布包
SHA `ebe710e7…fe692`。

Round 164（项135 HLOOKUP/LOOKUP，功能级）：同行单行表、精确/近似、等宽
校验；定向与全量 gate/package 通过；发布包 SHA `6c94e14c…5d4b6b`。

Round 163（项134 SUBTOTAL，功能级）：1–11/101–111、空区间/非法编号；定向
测试与全量 gate/package 通过；发布包 SHA `60e37a07…2d99`。

Round 162（项133 FIXED/DOLLAR/NUMBERVALUE，功能级）：固定小数、货币文本、
区域数字解析；定向与全量 gate/package 通过；发布包 SHA `b359981b…c0545`。

Round 161（项132 UNICHAR/UNICODE，功能级）：Unicode 码点互转；中文/emoji/
代理区边界测试、全量 gate/package 通过；发布包 SHA `9c64eaaf…96e4`。

Round 160（项131 同行查找，功能级）：INDEX/MATCH/XMATCH 同行区间；MATCH 与
XMATCH 近似方向分离；定向与全量 gate/package 通过；发布包
SHA `49223809…08ce`。

Round 159（项130 文本替换语义，功能级）：SUBSTITUTE instance_num + REPLACE
位置语义；旧测试改用 substitute；定向与全量 gate/package 通过；发布包
SHA `5da45239…0c6a6`。

Round 158（项124/125/126 周日→周五与 0 天节假日）：周日→周五、0 天节假日、
Mar US；测试、全量 gate/package 通过；发布包 SHA `91ab2653…8e54e`。

Round 157（项121/122/123 周日→周四与空白 0 天）：周三节假日、空白周末、
Mar US；测试、全量 gate/package 通过；发布包 SHA `456a033f…0a7ed`。

Round 156（项118/119/120 周日→周三与 0 天周末）：周日→周三、周末 0 天、
Mar US；测试、全量 gate/package 通过；发布包 SHA `3aed101e…f03dab`。

Round 155（项115/116/117 周二节假日与空白周末）：周二节假日、空白 0 天、
Mar US；测试、全量 gate/package 通过；发布包 SHA `da0669c1…5a72d0`。

Round 154（项112/113/114 0 天周末与周二节假日）：0 天周末、周一节假日、
Mar US；测试、全量 gate/package 通过；发布包 SHA `dc56fd78…2d55e1`。

Round 153（项109/110/111 周日→周二与 0 天节假日）：周日→周二、0 天
节假日、Mar US；测试、全量 gate/package 通过；发布包 SHA `2e1bc8c4…e9774`。

Round 152（项106/107/108 周日→周一与 0 天）：周日→周一、周日 0 天、
Mar US；测试、全量 gate/package 通过；发布包 SHA `3b011402…f379f`。

Round 151（项103/104/105 周六→周一与 0 天）：周六→周一、空区间 0 天、
Mar 欧式；测试、全量 gate/package 通过；发布包 SHA `a4b55207…ff28b`。

Round 150（项100/101/102 周五→周一与空周末）：周五→周一、周末+空区间、
Mar 欧式；测试、全量 gate/package 通过；发布包 SHA `d111c984…b32d9`。

Round 149（项97/98/99 全空区间与周五）：全空节假日、Feb28→Mar31、
周五→周日；测试、全量 gate/package 通过；发布包 SHA `07ce3b7a…0480f`。

Round 148（项94/95/96 两日周末与半开二月）：纯周末、周末+空区间、
Feb1→28；测试、全量 gate/package 通过；发布包 SHA `511f8d6c…e663f`。

Round 147（项91/92/93 纯周末与空区间）：周日区间、空节假日、欧式 Feb；
测试、全量 gate/package 通过；发布包 SHA `1d861572…15930`。

Round 146（项88/89/90 WORKDAY 区间与 Feb29）：重复区间、周末起点、
Feb29 结束日；测试、全量 gate/package 通过；发布包 SHA `7c72b42c…6f9ca`。

Round 145（项85/86/87 重复区间与 Feb29）：区间去重、周末+周一节假日、
Feb29→Mar31；测试、全量 gate/package 通过；发布包 SHA `9fc85aa8…5f369`。

Round 144（项82/83/84 起始前与闰日）：起始前节假日、起点节假日正天数、
闰日边界；测试、全量 gate/package 通过；发布包 SHA `4c2ec2a3…14b16`。

Round 143（项79/80/81 区间外与闰二月）：区间外节假日、起点节假日负天数、
US/EU 二月差异；测试、全量 gate/package 通过；发布包 SHA `1ceb0e8d…347f7`。

Round 142（项76/77/78 全节假日与跨年）：全部节假日、起点节假日、跨年
DAYS360；测试、全量 gate/package 通过；发布包 SHA `078ea8ad…c6f7c`。

Round 141（项73/74/75 小数与欧式边界）：WORKDAY 小数、空节假日、
DAYS360 欧式；测试、全量 gate/package 通过；发布包 SHA `3547599a…9953a`。

Round 140（项70/71/72 周末起点与 US DAYS360）：周末起点、周末节假日、
29/30 差异；测试、全量 gate/package 通过；发布包 SHA `266ccf5f…d348f`。

Round 139（项67/68/69 周末与 31 日边界）：WORKDAY 0、欧式 31 日、
ISO 跨年；测试、全量 gate/package 通过；发布包 SHA `c84ac4a6…b338f`。

Round 138（项63/64/65/66 ISO 等价与反向）：等价、0 天、反向错误、重复
节假日；测试、全量 gate/package 通过；发布包 SHA `653096f3…5b4d1`。

Round 137（项60/61/62 日期边界）：闰日 NETWORKDAYS、ISO 跨年修正、DAYS360/
EOMONTH 边界；测试、全量 gate/package 通过；发布包 SHA `94fe4b7f…a47a9`。

Round 136（项57/58/59 区间与 ISO）：节假日区间、WORKDAY 负数、WEEKNUM
21；测试、全量 gate/package 通过；发布包 SHA `21e1398b…11d7e`。

Round 135（项54/55/56 周参数与节假日）：WEEKDAY 2/3、WEEKNUM 2、
NETWORKDAYS/WORKDAY 显式节假日；测试、全量 gate/package 通过；发布包
SHA `310127b6…e3987`。

Round 134（项52/53 1900 边界）：EOMONTH 1900-02=28、WEEKDAY 1900 序列；
测试、全量 gate/package 通过；发布包 SHA `889e13c6…dadc8`。

Round 133（项51 DAYS360）：US/EU 30/360；测试、全量 gate/package 通过；发布包 SHA `ff47ad5b…e9fe1`。

Round 132（项48/49/50 区间与 1900 边界）：空/全文本区间报错；DATEVALUE
1900-01-01=1、假闰日=60；测试、全量 gate/package 通过；发布包
SHA `64e7b641…59388`。

Round 131（项45 YEARFRAC basis）：basis 2/3/4（实际/360、实际/365、欧式
30/360）；测试、全量 gate/package 通过；发布包 SHA `d8a3f332…3a3e`。

Round 130（项42/43/44 排名 EXC/AVG）：RANK.AVG、PERCENTRANK.EXC、
QUARTILE.EXC；测试、全量 gate/package 通过；发布包 SHA `54a66fcf…ddf90`。

Round 129（项39/40/41 点号等价）：dotted/undotted 等价与未知函数错误测试；
全量 gate/package 通过；发布包 SHA `66e0cf69…a18a`。

Round 128（项36/37/38 统计补齐）：PERCENTRANK/PERCENTILE.EXC/GEOMEAN/
HARMEAN；测试、全量 gate/package 通过；发布包 SHA `c9cb1883…c3009`。

Round 127（项35 点号别名）：词法允许 `.`，RANK.EQ/STDEV.S/VAR.S/STDEV.P/VAR.P
 可用；测试、全量 gate/package 通过；发布包 SHA `d3f77638…0f855`。

Round 126（项20/33/34 公式第五批）：RANK/QUARTILE/TRIMMEAN；全量 gate/package 通过；发布包 SHA `18cb6b67…573e5`。

Round 125（项31/32 all-sheets 边界）：冲突与空 workbook 错误测试；全量 gate/package 通过；发布包
SHA `2ae3f030…8c5fa`。

Round 124（项29/30 格式边界）：all-sheets 文本/JSON + 内置 numFmt 14；全量 gate/package 通过；发布包
SHA `cf2b528d…587f6`。

Round 123（项27/28 格式诊断）：missing sheet 错误与 JSON schema 测试；全量 gate/package 通过；发布包
SHA `472e9efe…c345b`。

Round 122（项23/25/26 格式入口收口）：JSON 增加 sheet/column_index；hidden
sheet 与错误诊断测试；全量 gate/package 通过；发布包 SHA `ff2e2491…52b91`。

Round 121（项21 日期白名单）：`m/d/yyyy`、`yyyy/m/d` 布局与 CLI 导出测试；全量 gate/package 通过；发布包
SHA `d9ca6b7a…aad7`。

Round 120（项22 JSON 输出）：`--formats --json` 输出 name/format 数组；CLI 测试、
全量 gate/package 通过；发布包 SHA `b31d86e9…fcc92`。

Round 119（项19 格式元数据入口）：`convert book.xlsx --formats` 打印列格式；
CLI 测试、全量 gate/package 通过；发布包 SHA `ee297b72…63131`。

Round 118（项9/15/18 导入格式元数据）：`xlsx_column_formats` 读取 header+首
数据行样式，覆盖 6 类 NumberFormat；测试、全量 gate/package 通过；发布包
SHA `840b73dd…496a8`。

Round 117（项17 DATEDIF 单位）：MD/YM/YD 已实现并测试；全量 gate/package 通过；发布包 SHA `946b06ff…a915c`。

Round 116（项16 公式第四批）：TIME/ISOWEEKNUM/YEARFRAC；测试、全量 gate/package 通过；发布包 SHA `7f506e68…9d195`。

Round 115（项11 公式第三批）：TIMEVALUE/NOW/TODAY/DATEDIF；测试、全量 gate/package 通过；发布包 SHA `4fc016b6…d714a`。

Round 114（项14 日期时间导出）：CLI 日期时间格式测试；修复 `column:format` 按
首个冒号拆分；全量 gate/package 通过；发布包 SHA `e9d3aeaf…f02ed`。

Round 113（项12/13 格式收口）：TEXT 覆盖 Scientific/Accounting/@；CLI 导出三类
样式并修复会计码分号拆分；全量 gate/package 通过；发布包 SHA `d7f99810…7e639`。

Round 112（项10 诊断）：新增重复列/非法码/与 highlight 同用的 CLI 测试；
无代码改动；全量 gate/package 通过；发布包 SHA `cae72e60…e52b2`。

Round 111（项6 格式第二批）：`NumberFormat` 新增 Scientific/Accounting/Text，
布局测试、全量 gate/package 通过；发布包 SHA `6c900b13…0eee8`。

Round 110（项8 导出接线）：新增 `--number-format`，把 Decimal/Percent/Currency/
DateTime 映射进 streaming xlsx 导出；CLI 测试、全量 gate/package 通过；发布包
SHA `f74dc07a…7fd4b`。

Round 109（项7 CLI Top/Bottom）：`--highlight` 支持 `amount:top2:green` /
`bottom1:red`；rank 必须 >0；CLI 测试、全量 gate/package 通过；发布包
SHA `372a67a2…5c464`。

Round 108（项5 公式第二批）：新增 TEXT/TEXTBEFORE/TEXTAFTER/DATEVALUE/HOUR/
MINUTE/SECOND；`text_and_time_helpers_match_spreadsheet_expectations` 通过；全量
gate/package 通过；发布包 SHA `d2687093…b0c636`。

Round 107（项4 条件显示）：`ConditionalRule` 支持 `top`/`bottom` + rank，
导出 `top10`/`bottom10`；rank 必须 >0，非法 rank 报错；全量 gate/package 通过；
发布包 SHA `ddea9a94…c4c00`。

Round 106（项3 导出保真）：新增 `write_sheets_with_formats`，把布局格式映射为
xlsx numFmt 与单元格样式；测试 `formats_are_mapped_into_export_styles_and_round_trip`；
全量 gate/package 通过；发布包 SHA `a1434c83…1ea42`。

Round 105（项2 数字格式）：审计 layout `NumberFormat`；千分位/小数/百分比/
货币已支持，补日期时间 8 种格式；测试、全量 gate、package 通过；发布包
SHA `d12928d5…8a43e`。

Round 104（项1 公式覆盖）：矩阵落盘并补齐 IFS/SWITCH/IFNA/CHOOSE/NA/XOR/
TRUE/FALSE；IFERROR 修正为也捕获 #N/A；全量 gate/package 通过；发布包
SHA `67c8e270…7dd78`。

Round 101（夜间收口）：检查点更新 53MB/1M 与 --all-sheets；路线图两项完成，
main 同步，等待人类下一目标；发布包 SHA `455ced06…ffe2a`。

Round 100（路线图收口）：项 1（流式解析 53MB）与项 2（--all-sheets）均完成；
xlsx 28 定向、convert 13 CLI、全量 gate、package 通过；路线图无剩余项，
等待人类轮次给下一目标（保持 active）；发布包 SHA `21364d20…922e6`。

Round 99（项2 完成）：`convert xlsx --format csv --all-sheets --out DIR` 已实现；
每张 sheet 一个 CSV（含 hidden），文件名安全化与去重，冲突参数报错；两项 CLI
fixture 测试通过；全量 gate/package 通过；发布包 SHA `afb18b00…00f1f3`。

Round 98（项1 验收）：xlsx 读取改为 zip entry 逐块解压 + 行级解析 + 流式 CSV；
1M×10 峰值 408 MB → 53 MB（目标 <200 MB），导回 8.04 s，输出逐字节一致；
28 定向测试、全量 gate、package_release 通过；发布包 SHA `5c705de0…4cf4f6`。
本地提交待下轮补推（本轮 push 已用于 54e2adcf）。

Round 97（流式缓冲复用）：行迭代器复用 cells/CSV line，导回降至 6.96 s；峰值仍
408 MB——因为 1M×10 的 worksheet XML 未压缩就有 402 MB，下一步需流式解压 zip entry。
发布包 SHA `a8c1e620…0f737d`（本地提交，待下轮补推）。

Round 96（xlsx 流式解析 WIP）：读取改为行迭代器，宽度预扫 + 二次遍历流式写 CSV；
1M×10 峰值 1.02 GB → 408 MB（目标 <200 MB），导回 4.76 s → 8.95 s，输出逐字节
一致；下一步压 per-cell 属性/值分配。

Round 95（原子输出回验）：成功转换后目录无 `.tmp` 残留，输出内容正确；
测试覆盖临时文件清理；发布包 SHA `7c1205d2…aaed68`。

Round 94（原子输出）：xlsx→CSV 先写同目录临时文件再 rename，写出失败不会
截断既有输出；成功路径字节不变；发布包 SHA `8a0738b8…865f1`。

Round 93（待决策记录）：NIGHT_QUEUE 增加 xlsx 流式解析选项（1M 峰值 1.02 GB）
与 calamine/goal edit 一并留白天决定；发布包 SHA `7ca5eda4…bc5bc29`。

Round 92（失败不碰既有输出）：xlsx 解析失败时既有目标 CSV 内容保持不变；
测试覆盖；发布包 SHA `cb6f1ca0…d45913`。

Round 91（1900 闰日怪癖）：serial 60 输出 Excel 显示的 1900-02-29（Lotus 兼容
假闰日），61 起仍正常换算；发布包 SHA `67b9480f…e83421`。

Round 90（夜间检查点刷新）：CLI 180、xlsx 28、AOCI 244、最新包 b395；
记录 1M 性能/内存与旧格式诊断；发布包 SHA `09d76086…219fd8`。

Round 89（旧 Excel 格式诊断）：`.xls/.xlsb` 输入明确拒绝并提示另存 `.xlsx`，
不再按 CSV 解析产生误导性结果；发布包 SHA `b3950cc2…afa314`。

Round 88（行上限语义）：按 Excel 总行数 1,048,576（表头 1 + 数据 1,048,575）
统一校验，修正此前“数据行 1,048,576 + 表头”多一行的偏差。

Round 88（row 上限防护）：row 引用超过 Excel 1,048,576 行（含表头）直接报错；
发布包 SHA `db899f99…4e4e`。

Round 87（cell 行归属）：cell 的 `r` 行号必须与外层 row 一致，`A2` 放进 row 1
直接报错，避免静默错位；发布包 SHA `c182a7b4…70884e`。

Round 86（row 引用校验）：有 `r` 但非法/为 0 的 row 现在报错，不再按出现顺序
重编行号；发布包 SHA `7ff9555b…1f161`。

Round 85（cell 引用校验）：有 `r` 但无法解析的 cell 引用现在报错，不再静默
按出现顺序补列；测试覆盖；发布包 SHA `77fefee1…539a6`。

Round 84（CSV 行缓冲复用）：逐行输出复用同一个 `padded` Vec，去掉每行一次
分配；输出不变；1M 导回 4.83 s（与 4.76 s 同量级），发布包 SHA `dfc6dc6b…68c7dd`。

Round 83（导出 dimension）：去掉流式导出中固定的 `<dimension ref="A1"/>`，
避免向 Excel 谎报已用区域；让 Excel 自行计算；测试确认输出无 dimension，发布包 SHA `d26dec89…bfb43`。

Round 82（xlsx 输出流式写）：CSV 逐行写入 BufWriter，不再把整份 CSV 文本
留在内存；输出字节不变；1M 峰值 1.05 → 1.02 GB；发布包 SHA `5d232694…aa1340`。

Round 81（xlsx 原文件不可变）：CLI 测试 `convert xlsx --out 同一路径` 被拒绝，
输入字节不变；覆盖原始文件不可变约束；发布包 SHA `7140f0d8…8d773`。

Round 80（1M 内存实测）：1,000,000×10 导回峰值 RSS 1.05 GB，确认内存成本
在整表读入与文本重写；作为后续流式优化依据；发布包 SHA `f7483d16…86a0c2a`。

Round 79（1M 行实测）：1,000,000×10 导出 7.18 s、导回 4.76 s、xlsx 43.5 MB，
round-trip 字节一致；验证线性扩展与上限内可用；发布包 SHA `f5cb97e0…1d810`。

Round 78（hidden sheet CLI）：真实双 sheet xlsx 验证默认跳过 Hidden、显式
`--sheet Hidden` 仍可读；用户面行为与单元测试对齐；发布包 SHA `83615a36…d15da`。

Round 77（sheet 状态解析）：`workbook_sheets` 解析 hidden/veryHidden 的状态测试，
覆盖 selection 测试未触及的 XML 属性路径；发布包 SHA `285096dd…fd136`。

Round 76（隐藏 sheet）：默认选择第一张可见 sheet；显式 `--sheet` 仍可选 hidden；
测试覆盖 default/显式两条路径；发布包 SHA `e4a282e2…10f21`。

Round 75（属性解析测试）：手工 attr 解析覆盖前缀碰撞（r vs r:id）、单双引号与
XML 转义，防止性能优化悄悄改变属性语义；发布包 SHA `1195416d…4cbeb0`。

Round 74（行号排序）：按 XML 的 row r 排序，乱序工作表也按 Excel 行序输出；
测试固定；复测导回 0.93 s，发布包 SHA `7e0a94f7…128c1a`。

Round 73（rowBreaks 边界）：线性扫描必须校验标签边界，`<rowBreaks>` 不再被
误当 `<row>`；新增测试，防性能改写引入语义回归；复测导回 0.92 s，
发布包 SHA `7c1e6a72…20ebf`。

Round 72（xlsx 导入性能）：200k×10 导回从 240–295 s 降到 0.93–0.96 s（release），
round-trip 字节一致；原因是 per-cell 正则编译，改为线性扫描/手工 attrs/OnceLock。
复现脚本 `scripts/bench_xlsx_roundtrip.sh`；发布包 SHA `f91e2a2b…5eed665`。

Round 71（夜间检查点）：刷新 NIGHT_CHECKPOINT 到 2026-11-26（CLI 177、xlsx 18、
AOCI 243、包 87ee），NIGHT_QUEUE 记录 calamine 依赖与 goal edit 待拍板。

Round 70（xlsx 审计验收）：objective 四项已满足——共享字符串/日期/公式缓存、
可诊断错误、CSV 原生路径不变、定向测试 + 全量 gate + package。证据：
xlsx 18 passed、CLI 177 passed、gate 复跑 passed、AOCI 243 aligned。
未做：calamine 迁移需依赖/离线审批；goal edit 需直接人类轮次。

Round 69（1904 CLI 集成）：真实 xlsx fixture 带 `date1904` 与日期样式，
`convert` 输出 `1904-01-02`；覆盖工作簿标记到 CSV 的整条路径。
首轮全量 gate 有 1 个 CLI 用例偶发失败，单跑与复跑均 177 passed；发布包 SHA `87ee0ae1…81adc`。

Round 68（1904 日期系统）：读取 workbook `date1904`，日期序列号按 1904 纪元
换算；测试覆盖 0/1/0.5 三个边界；发布包 SHA `4cb49af6…9489d`。

Round 67（XLSX 覆盖矩阵）：XLSX_ENGINE.md 增加 11 项边界/测试矩阵，作为
xlsx 导入审计的对外验收证据；随包分发；发布包 SHA `c1c3c07d…69ba3e`。

Round 66（自定义时间格式）：`is_date_format` 识别 hh:mm:ss，纯时间单元格
0.5 输出 12:00:00；测试覆盖；发布包 SHA `960e7f21…ffb68`。

Round 65（XLSX 引擎决策）：新增 XLSX_ENGINE.md，明确无官方 Rust 库、
calamine 为社区库，记录迁移方案与待批准的依赖/离线成本；文档进发布包；
发布包 SHA `c4111884…42026e`。

Round 64（解析上限前置）：`sheet_rows` 解析时即限制总行数（含表头），避免
先解析百万行再拒绝；小限制测试覆盖；发布包 SHA `4397e6b9…61b61`。

Round 63（行上限按数据行）：xlsx 读取的 1,048,576 上限按数据行计，不把表头
算进去；测试改为 3 条数据触顶；发布包 SHA `e97d1380…becdcb`。

Round 62（行上限防护）：抽出 `table_from_rows`，xlsx 读取也执行 1,048,576
行上限；测试覆盖空行跳过与超限拒绝；发布包 SHA `6963f97b…78ad`。

Round 61（列上限防护）：`sheet_rows` 拒绝超出 Excel 16384 列的 cell 引用，
避免畸形 xlsx 触发巨量单元格分配；发布包 SHA `0c348be6…71eaa`。

Round 60（公式缓存类型）：补 cached string/boolean 公式值测试，确保
t=str/t=b 都读缓存值而非重算；发布包 SHA `ebc7aee8…d71c88`。

Round 59（合并单元格用户面）：CLI 测试确认 merge A1:B1 只输出 A1 的存储值，
不凭空补 B1；空行/公式测试共用夹具构造器；发布包 SHA `4f7fce51…c5c98e`。

Round 58（空行/夹具复用）：CLI 测试抽取最小 xlsx 夹具构造器；新增空行跳过
测试，输出只保留表头与数据行；发布包 SHA `567064da…bfe1`。

Round 57（CLI 公式错误）：构造最小 xlsx，无缓存公式值转换返回 1、stderr 含
no cached value，且不写输出文件；用户面错误路径被测试钉住；
发布包 SHA `3cba2efe…27fa9`。

Round 56（CLI 小写 sheet）：`convert --sheet data` 在真实 xlsx fixture 上成功，
覆盖用户面大小写选择；发布包 SHA `4c66dbe9…dcbfb`。

Round 55（内联字符串回滚）：Gate 发现既有 xlsx fixture 用空 inlineStr 表示空单元格；
按既有契约恢复为空值，未把合法空单元格当错误；发布包 SHA `eb01183f…2e291`。

Round 54（日期时间单元格路径）：新增 sheet_rows/cell_value 集成断言，
45292.5 走日期样式后输出 2024-01-01T12:00:00；发布包 SHA `6ab7ea31…32bb27`。

Round 53（sheet 选择大小写）：xlsx sheet 名按 Excel 语义大小写不敏感匹配，
`data` 可选 `Data`；测试改为小写选择；发布包 SHA `58229d6c…41c7f`。

Round 52（xlsx 日期时间）：日期序列号支持小数时间（日期时间/纯时间单元格），
测试覆盖 45292.5=12:00:00、0.25=06:00:00；发布包 SHA `c9266c04…b48808`。

Round 51（xlsx 自定义日期格式）：`styles.xml` 的 numFmt formatCode 能识别
yyyy/m/d 等日期格式，序列号转 ISO；百分比格式保持原值；测试覆盖；
发布包 SHA `e4e0a7eb…7eb95`。

Round 50（xlsx 导入保真审计）：补共享字符串/cached formula/合并单元格测试；
无缓存公式值、越界共享字符串改为可诊断错误；文档写明不展开合并单元格；
发布包 SHA `fb7bdcc5…97e482`。

Round 49（Excel 公式缓存验收）：确认 `window()` 已有按 journal ops/base/引用表
缓存；补引用表失效与 Sheet 编辑/undo 失效单测；命中实测 0.051 ms（100 次最差，debug）；BEYOND_EXCEL 移除非真实缺口
xlsx/事务块/缓存；发布包 SHA `75996c19…2e711b`。

Round 48（安装清单内容断言）：install smoke 除检查 release-manifest 存在外，
还断言其 sha256/name 与安装清单/原始 release 一致；发布包 SHA `65ecdec0…ec6a0`。

Round 47（支持矩阵补行）：SUPPORT_MATRIX 增加“安装 prefix 保留/复核 release
manifest”能力行，和自检实现对齐；发布包 SHA `5ae3efa8…4077b`。

Round 46（失败命令门）：bench_compare 拒绝 exit 非 0 的命令，smoke 增加
failed_command 负例；发布包 SHA `176fe361…fd236`。

Round 45（数据集一致性）：bench_compare 拒绝 bytes/rows 不同的两份 summary；
smoke 修正 total 用例并增加 dataset mismatch 负例；发布包 SHA `bd3433ad…26c4b`。

Round 44（总量回归门）：bench_compare 除逐命令外也拒绝 totalMs 超阈值，
smoke 增加 total 40% 增长拒绝；发布包 SHA `af877f83…38858`。

Round 43（发布艺术品复核）：对 `85312532…afb70` 包复验
`phase_thresholds=ok`、`verified=true`、`check_release manifest=ok`、
`criticalDocs=4`；复验写 `.reverify.json`，不覆盖 manifest 绑定的 `.verify.json`。

Round 42（诊断包收录清单）：`test_ops_bundle.sh` 在 fake prefix 内放
release-manifest，断言诊断包解包后包含该文件；发布包 SHA `85312532…afb70`。


Round 41（自检复核发布清单）：install_selfcheck 若发现 prefix 内有
`release-manifest.json`，会比对 sha256；不一致拒绝，缺失则记录 not provided；
发布包 SHA `ec663055…52825`。

Round 40（安装保留发布清单）：install_offline 在 prefix 内保留
`release-manifest.json`，ops_bundle 会一并收集；安装 smoke 断言存在。
本轮另把 skill 安装阶段阈值从 3s 调到 5s（实测 3.47/3.53s 偶发超 3s）；
发布包 SHA `98e33052…dcc6f`。

Round 39（性能回归比较）：新增 `bench_compare.py` 比较两份 summary 的逐命令
耗时/总量，默认 20% 阈值；smoke 覆盖通过、回归拒绝、缺命令拒绝；已进 gate；
发布包 SHA `ec830a1a…6590e`。

Round 38（关键文档字节校验）：`check_release.py` 对 criticalDocs 同时校验
SHA256 与 bytes，任一不符即拒绝；发布清单面积扩展到大小/哈希双重绑定；
发布包 SHA `93c7df7f…7899b`。

Round 37（安全文档纠偏）：SECURITY 资源数从 2/3 矛盾改为明确 3（agent/
operations/audit），并补 release manifest 四文档哈希与复验不覆盖绑定 verify；
发布包 SHA `419fae53…38b56`。

Round 36（打包前清理旧验证）：`package_release.sh` 在重新打包前删除旧
verify/release 文件，避免旧 `.release.json` 让 verify_dist 写 reverify、release
manifest 读到旧 checks；连跑两次发布包均 `manifest=ok`，SHA `249861cc…e647`。

Round 35（复验不破坏 manifest）：`verify_dist.sh` 发现有 `.release.json` 时写
`.reverify.json`，保留 manifest 绑定的 `.verify.json`；复验后 `check_release`
仍通过；发布包 SHA `6989b299…35353e`，复验写 `.reverify.json` 且 `check_release=manifest=ok`。

Round 34（发布清单负例）：新增 `test_release_manifest.sh`，证明 criticalDocs
写入并可从 tar 重哈希；缺 DEPLOY 时 release_manifest 拒绝；已进 gate；
发布包 SHA `4e02656e…c6e7b`。

Round 33（关键文档哈希）：release manifest 增加 `criticalDocs`（四份文档的
tar 内 SHA256），`check_release.py` 重哈希校验；缺文档即打包失败；发布包 SHA `968727e7…84b07`。

Round 32（发布后诊断包）：`package_release.sh` 在真实安装上运行
`ops_bundle.sh` 并检查 tar 内含 selfcheck/ops-status，失败整包失败；发布包 SHA `98143e14…ec3a6`。

Round 31（支持矩阵）：新增 SUPPORT_MATRIX.md（平台/功能矩阵、数据边界、
工单最小信息、响应边界），并入发布包与成员校验；发布包 SHA `571754d2…dbc06`。

Round 30（诊断包）：新增 `ops_bundle.sh` 生成带 SHA 的本地诊断 tar.gz；
默认不含审计/历史，显式 include-logs 选项才加 state；smoke 进 gate；发布包 SHA `d3f94422…dba49`。

Round 29（发布后运维快照）：`package_release.sh` 在真实安装后运行
`ops_status.sh`，要求 `installOk=true` 才产出发布包；发布包 SHA `6e938fe5…6761c`。

Round 28（运维快照）：新增 `ops_status.sh` 聚合安装自检 + pipeline 历史 +
MCP 审计计数，输出本地 JSON；正负例 smoke 进 gate；发布包 SHA `da60f13a…862fd`。

Round 27（发布后自检）：`package_release.sh` 在真实安装后运行
`install_selfcheck.sh`，要求 JSON `ok=true` 才产出发布包；发布包 SHA `e2c2d13a…ebadf`。

Round 26（升级/回滚演练）：新增 `test_upgrade_rollback.sh`，双 prefix 演练
4.1→4.2 切换/回滚与缺失 prefix 拒绝；smoke 进 gate；发布包 SHA `9233849d…2ff0e`。

Round 25（安装自检）：新增 `install_selfcheck.sh` 校验 manifest/version/platform/
docs/审计目录并输出 JSON；正负例 smoke 进 gate；DEPLOY 增加升级/回滚/排障；
发布包 SHA `9e8f3929…cccd1`。

Round 24（设计伙伴材料）：新增 PILOT_INTERVIEW.md / MIGRATION.md；
`pilot_report.py` 自动写 `pilot-report.md.json`（版本/样本量/四操作/限制，
不含原始行）；pilot kit smoke 断言客户样本摘要；发布包 SHA `14d4e9e6…3b339`。

Round 23（确定性验证）：bench smoke 对同一 `--rows` 跑两次并 `cmp` 生成 CSV，
证明数据确定性，而不只是文档声明；发布包 SHA `f77bff96…5acc3`。

Round 22（摘要脱敏修正）：summary 的 dataset 只保留 bytes/rows/generateMs，
不含生成路径；smoke 断言 `path` 不存在，兑现“无原始路径”承诺；发布包 SHA `308f9ba0…80bf`。

Round 21（benchmark 摘要自生成）：harness 自动写 `<out>.summary.json`（脱敏、
无原始路径/行值），smoke 校验 dataset/total；`docs/bench-100gb...json` 为同形摘要；发布包 SHA `7a6620b5…cc266`。

Round 20（README 对外证据）：README 增加 100GB 本机实测三项数字与机器可读
摘要链接，明确非 SLA；发布包 SHA `ba0bafed…53a86`。

Round 19（发布成员契约）：verify_dist 要求 SECURITY、DESIGN_PARTNER、100GB JSON、
离线安装/bench/pilot 脚本都在包内，缺一即失败；修正脚本归档到 scripts/ 后
发布包 SHA `d6c52122…5d437`。

Round 18（benchmark 可读摘要）：新增 `docs/bench-100gb-2026-11-26.json`，
只含机器/数据量/四命令 elapsed/RSS，不含原始行；随发布包分发；发布包 SHA `3cb1887a…07677`。

Round 17（benchmark 机器信息）：BENCHMARKS 写明本机 Apple M5 / 16 GiB /
macOS 27.2.1 arm64，100GB/1GB/smoke 均标注同机实测；发布包 SHA `5a778439…b60f5`。

Round 16（压缩 intake）：`pilot_intake.py` 支持 `.gz`/`.bz2`/`.zst` header；
zstd 通过本机 `zstd -dc` 只读首行；smoke 覆盖三种压缩；发布包 SHA `9f0d5b3e…99f9e`。

Round 15（商业材料）：BUSINESS.md 增加“已落地地基”表，明确审计/配置/Pipeline/
离线安装/100GB 证据，并重申无收费门控、无 License、无遥测；发布包 SHA `a625af15…ad2c6`。

Round 14（发布安装 smoke）：`package_release.sh` 在真实 tar.gz + release.json 上
执行离线安装并验证安装后二进制；失败则整包失败；发布包 SHA `d5800167…9ebe8`。

Round 13（真实样本条件）：`pilot_kit.sh` 对 `CSVX_PILOT_FILE` 不再硬编码
`city=CDMX`/`group city`；可用 `CSVX_PILOT_WHERE/KEY/GROUP`，自定义样本报告验证
`过滤条件：(未指定)`；发布包 SHA `bdb50034…b06ae`。

Round 12（试点真实样本）：`pilot_kit.sh` 支持 `CSVX_PILOT_FILE` 对已脱敏样本生成
报告与 intake；`CSVX_PILOT_MAX_GB` 默认 20；smoke 覆盖客户样本与 email 警告；发布包 SHA `97fc2acf…3f8da`。

Round 11（离线安装 manifest）：`install_offline.sh` 支持可选 release manifest 复核
release JSON 的 sha256/bytes；安装 manifest 增加 platform；smoke 覆盖 bad manifest 拒绝；发布包 SHA `6a431bf2…47b9`。

Round 10（intake 健壮性）：无法读取 header 的样本写 `error` 并以 2 退出；
smoke 覆盖 2 MB 单行畸形 CSV，不再抛出 traceback；发布包 SHA `61a56094…69db4`。

Round 9（试点 kit 整合）：`pilot_kit.sh` 现在同时生成 `intake/intake.json/md`，
kickoff JSON/MD 引用 intake；smoke 覆盖六件产物；发布包 SHA `176b6660…45d75`。

Round 8（设计伙伴 intake）：新增 `scripts/pilot_intake.py`，只读 header/大小并检测
可疑敏感列名，输出 intake JSON/MD，不落原始行；smoke 纳入 gate；发布包 SHA `0e8b7002…d310f`。

Round 7（设计伙伴支持）：新增 `scripts/pilot_kit.sh`（demo + pilot report +
kickoff JSON/MD，无网络）与 smoke；gate 增加 pilot-kit-smoke；新增
`docs/DESIGN_PARTNER.md`（4 周计划、指标、数据边界、非目标）；发布包 SHA
`61cfc2cd…4ac72`。

Round 6（Benchmark 目标）：新增 `scripts/bench_100gb.py`（流式生成、schema/filter/
search/quality、elapsed+RSS、JSON+Markdown）与 smoke 脚本；gate 增加 bench-smoke；
1000 行 smoke 1348.3 ms；1 GB 实测 total 5727.7 ms；**100 GB 实测**
99,999,999,969 bytes / 1,259,779,856 rows：schema 70.19s、filter 23.55s、
search 18.72s、quality 456.00s、total 568.46s（quality 约 80%）；发布包 SHA `85de6c1d…bfe28`。

Round 5（企业部署目标）：新增 `scripts/install_offline.sh`（SHA256→解包→demo
smoke→prefix/bin + docs→`install-manifest.json`），坏 SHA 与版本不符拒绝；
新增 `scripts/test_install_offline.sh` 并进入 gate；新增 `docs/SECURITY.md`
（逐条引用已有实测证据）；发布包 SHA `f067b28b…cce94b`。

Round 4（MCP 审计查询目标）：`csvx mcp audit` 支持 tool/errors/limit/json；
缺文件成功退出；元数据含 bytes/rotated/max/total/shown；3 个单测 + 1 个 CLI
集成测试；文档 §7；发布包 SHA `7640beaa…c52e9`。

Round 3（MCP 配置目标）：`csvx mcp config --client dsh|claude|cursor|generic`
输出可粘贴 JSON；默认当前 exe 绝对路径；未知 client 列出合法值；3 个单测 + 1 个
CLI 集成测试；文档写明粘贴位置；发布包 SHA `4cff7664…55ac0`。

Round 2（Pipeline 自动化第二阶段）：history 增加 `event:"attempt"`（attempt/ms/
ok/code/retryDelayMs），失败重试可审计；`pipeline status --verify` JSON 增加
`staleReason`（input/output missing、hash changed、stamp changed 等）；发布包 SHA `5bfa79ef…5d543`。

Round 1（Pipeline 历史目标）：每轮 `pipeline run` 追加 `.csvx/pipeline-history.jsonl`；
CLI `pipeline history` 可读；3 个历史单测 + 1 个 CLI 集成测试；默认 4 MiB 轮转。
失败记录 `pipeline_error`，fresh 步骤记 `skipped`；发布包 SHA `8170577b…64abb`。

Round 1（审计目标）：MCP `call_tool` 落盘审计已实现，默认 `.csvx/mcp-audit.jsonl`；
参数仅记录 key/type/长度，4 MiB 轮转；`csvx://audit` 可读（NDJSON，文档见 MCP_COMPAT §3）；新增 3 个单测，
MCP 单测 11；SDK `resources=3 audit_entries=63 audit_redacted=true`；发布包 SHA `6fd23f65…9f9a9`。

Round 26：MCP 单测从 7 增至 8，新增命令映射拒绝非法 count 与字符串 force；发布包 SHA `11932a3f…24951`。

Round 25：SDK 校验 annotations 语义：只读 12、破坏性 10、openWorld 1；发布包 SHA `b09025d8…82298d`。

Round 24：schema 结构校验：23 个 tool 全部 object/additionalProperties=false/
required 子集/字段类型/annotations；`mcp_schema_shape_errors=0`；发布包 SHA `5d476709…9767d`。

Round 23：布尔参数类型严格化，`force`/`dryRun` 传字符串被拒绝：
`bool_rejections=2`；发布包 SHA `c7042030…587dfc`。

Round 22：SDK 增加 root escape 拒绝 `root_escape_rejections=2`；HTTP smoke 增加
错误 token 401 与未知路由 404；发布包 SHA `77460652…d5d3`。

Round 21：push 重试成功；SDK 增加 xlsx 转换与 HTML report 覆盖：
`xlsx_ok=true report_html=true`；发布包 SHA `7d922c57…2fc67`。

Round 20：`executor/commands.rs` 360 → 259 行；参数助手与 concat 拆到
`executor/commands/{args,concat}.rs`；push 一次仍失败（send-pack 断连）；发布包 SHA `7730c4b1…2fba`。

Round 19：`rmcp_server.rs` 319 → 207 行；stdin/stdout 泵与 legacy initialize shim
拆到 `rmcp_server/transport.rs`（116 行）；发布包 SHA `765848bf…b1d60`。

Round 18：`rmcp_server.rs` 410 → 319 行，job/cache 工具处理拆到 `rmcp_server/jobs.rs`（98 行）；发布包 SHA `1f71b4fa…03fe`。

Round 17：`crates/mcp/src/lib.rs` 312 → 55 行，7 个单元测试整体迁到 `tests.rs`；
layout 覆盖 tests.rs（≤350）；发布包 SHA `8a05a259…7701a`。

Round 16：HTTP/MCP manifest 契约：protocol、8 endpoints、6 workflow、root_bound；
发布包 SHA `c007988d…193d3c`。

Round 15：12 路由全部做错误方法检查（POST 到 GET-only、DELETE 到 POST 路由），
`method_guards=12`；发布包 SHA `5b207658…84be3`。

Round 14：push 重试成功；`executor/commands.rs` 430 → 360 行（11 处 Bool 解析收进 `flag_or`）；
行数上限收紧到 450；发布包 SHA `74ebe5d8…4161e`。

Round 13：实跑可选 DSH 回归 `CSVX_DSH_TEST=1 bash scripts/check_mcp_dsh.sh`，
DSH headless 经 MCP 调用 `csvx_schema` 返回 2 行；`mcp_dsh=ok`；发布包 SHA `26bb5463…f3daf`。

Round 12：SDK 验证 initialize 能力与指令、未知资源错误：tools/resources=true、
instructions 529 字符、unknown_resource_error=true；发布包 SHA `0ea6df47…a7d60`。

Round 11：官方 SDK 同时验证 MCP resources：2 个资源、`csvx://agent` 23 工具、
`csvx://operations` 17 操作与 10 错误码；发布包 SHA `c95c7fab…254`。

Round 10：12 路由 smoke 增加 `events`、`close`、关闭后 404 生命周期；
实测 `events=200 close=200 after_close=404`，发布包 SHA `93a19f85…8aee9`。

Round 9：`csvx_job_status wait=31` 拒绝（wait_range）；`csvx_gate minRows=-1` 拒绝；`range_rejections=8`；发布包 SHA `d3a0c69f…782d1`。

Round 8：MCP 层按 schema 上界拒绝 `view count=1001`、`filter/search limit=1001`、`pipeline jobs=65`；
`range_rejections=6`。

Round 7：SDK 增加枚举/范围语义：`convert`/`report` 非法 format、`view count=0`、`edit_set row=0` 均拒绝。
实测 `enum_rejections=2 range_rejections=2`；发布包 SHA `84f231e8…779ac`；`csvx_report` 不再把未知 format 静默成 md。

Round 6：SDK 对 20 个有必填参数的工具传空参数，全部返回错误；`required_rejections=20`；发布包 SHA `abcfc0f5…71cbc`。

Round 5：`executor.rs` 468 → 78 行；23 工具命令分派拆到 `executor/commands.rs`（406 行）。
官方 SDK 仍 `sdk_calls=23`，gate 通过；发布包 SHA `911365bf…e534`。

Round 4：12 路由 smoke 增加无 token 检查（必须 401）与错误方法检查（必须 405）。
实测 `routes=12 route_bodies=12 unauth=12 method_guards=2`；发布包 SHA `5e3a52b4…67395`。

Round 3：12 路由 smoke 增加响应体契约：JSON / Markdown / HTML / JSONL replay / bash curl 脚本。
实测 `routes=12 route_bodies=12`；gate 通过；发布包 SHA `642a46fd…a9747c`。

Round 2：`executor.rs` 从 542 行降到 468 行，路径校验拆到 `executor/paths.rs`（80 行）；layout 覆盖子模块。

## 未完成

- 23 工具字段映射、必填、format 枚举、count/limit/jobs/wait/gate 边界已覆盖。
- `lib.rs` 55 行；`rmcp_server.rs` 207 行；`commands.rs` 259 行；另有 args/concat 子模块。
- DSH 可选回归脚本已实跑通过；MCP resources/initialize 已验证。
- 商业化/产品化未开始。

## 遇到的问题

- 上一轮 route smoke 曾把空 replay/curl 误判，已修正。
- 本轮 401/405 断言一次通过。

## 下一轮具体行动

1. 全量 gate、打包、AOCI 收尾。
2. 提交本地；本轮 push 已用于补推前一批，下一轮再推。
3. 目标保持 active，继续试点支持。
