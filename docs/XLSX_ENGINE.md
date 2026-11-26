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
- 错误：无缓存公式值、坏共享索引、超行/列均显式报错，不静默造值。

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
导出 7.18 s；导回 4.76 s；round-trip 字节一致；仍在 Excel 1,048,576 行上限内（含表头）。
导入峰值内存实测 1.02 GB（macOS `/usr/bin/time -l`；CSV 改流式写后，此前 1.05 GB），
成本集中在整表读入。

同 200k 数据集在优化前导回需 240–295 s。原因是每个 cell/attribute 都重新编译正则；
改为线性标签扫描 + 手工属性解析 + `OnceLock` 缓存值正则后降到 0.93 s，差异约 250×。

`convert` 是用户可跑的入口；`read_xlsx_to_csv` 的错误会保留为 stderr，失败不写输出文件。
