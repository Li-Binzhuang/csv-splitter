# 架构 / Architecture

这份文档回答一个问题：**改动某处时，还有哪些地方会受影响。**

## 分层 / Layers

```
文件格式        base.csv    + .journal     + .idx        + .view
                 原始不变      操作日志        稀疏索引       显示元数据
                    │            │              │              │
核心 (core)     ┌───┴────────────┴──────────────┴──────────────┴───┐
                │  workbook（编辑模型）  index   filter  sort  ...  │
                │  csv_reader（读写记录）  encoding（编码）          │
                └───┬───────────────────────────────────────────┬──┘
                    │                                           │
入口            CLI (crates/cli/src/main.rs)        HTTP API (serve.rs)     GUI (冻结)
                    │                                           │
                tty / 脚本                                  浏览器 / 前端
```

**能力全部沉在 core**，CLI 和 API 只是两个入口。GUI 与它们共用 core，行为不会漂移。

## 文件格式 / On-disk format

| 文件 | 内容 | 谁写 | 关键约定 |
|---|---|---|---|
| `x.csv` | 原始数据 | **没人写**（不变式①）| 任何命令前后逐字节相同 |
| `x.csv.journal` | 追加式操作日志（CSV 记录）| `workbook::Journal` | 逐行追加 → 中断只损失最后一条 |
| `x.csv.journal.redo` | 被撤销的操作 | 同上 | 有它才能 redo；`push` 时清空 |
| `x.csv.idx` | 每 1000 行一个字节偏移 | `index::SparseIndex` | 校验不通过就重建 |
| `x.csv.view` | 列宽/显示名/冻结/数字格式 | `layout::Layout` | 坏掉就回退默认值 |

**journal 记「单元格是什么」，view 记「单元格长什么样」。** 两者分开，是因为它们的
变化频率差三个数量级：日志随每次编辑增长，布局一年改两次。

## 编辑模型 / The editing model

这是整个项目最需要小心的地方，改动 `workbook/rows.rs` 前请读完本节。

```
   journal 操作序列
        │  compile()  ──►  RowMap（逻辑行 → 物理行/插入块）
        │                 CellEdits（按行身份存的单元格编辑）
        │                 ColumnPlan（公式列、分列片）
        ▼
   RowPipeline::row()  ──►  一行最终值
        │
        ├─ 基础投影（按列布局从 base 取值）
        ├─ 分列片（从 base 文本派生）
        ├─ 区间填充（按行身份命中）
        ├─ 显式单元格编辑（按行身份命中，优先级最高）
        ├─ 批量规则（set --where）
        └─ 公式列（最后求值，可读前面所有结果）
```

**两条身份轴：**

- **行身份** `RowId::Base(n) / Virtual(id)`：插入/删除行后，编辑仍跟着原行走。
- **列身份**：按列名。改名时 `compile()` 会把已有编辑和公式重写过去（`rename_identifiers`）。

**派生的东西从哪读**（三处曾经因此出错，0.90/0.79/1.5）：

| 派生 | 读哪个值 |
|---|---|
| 分列片 | **base 文本**，不是可能已被编辑的源单元格 |
| 区间填充 | **行身份**，不是位置（位置语义会在删行后滑位）|
| 公式 / lag / running_total | **当前行的完成态**（含编辑）|

## 读路径 / Read paths

同一个窗口有三条路，它们**必须给出同样的字段和同样的值**（有测试）：

| 路径 | 什么时候走 | 实现 |
|---|---|---|
| 原始窗口 | 没有编辑、没有 `where`/`order` | `viewer::WindowViewer`（索引跳转）|
| 套用编辑的窗口 | 有 journal | `workbook::Pager` + `RowPipeline` |
| SQL 窗口 | 有 `where` / `order` / `sql` | DuckDB（有编辑时先物化）+ `cache::ResultCache` |

**成本**：前两条是常数时间；第三条每个不同的查询扫一次文件，结果进缓存（20 万行/条，40 万行总量）。

## 模块地图 / Module map

core 里 40 个模块，按职责分组（文件名即模块名）：

| 组 | 模块 |
|---|---|
| **编辑模型** | `workbook/{mod,rows,journal,export}` |
| **公式引擎** | `formula/{mod,functions,state,sheetref}`（表达式求值、函数表、`lag` 的行状态、跨表引用）|
| **读写文件** | `csv_reader` `encoding` `index` `viewer` `preview` `tempdir` `types` |
| **查询与变换** | `filter` `search` `sort` `dedupe` `reshape` `concat` `lookup` `groupby` `aggregate` `pivot` `chart` `profile` `validate` `format` `layout` `sql` `sql_cache` |
| **切分与压缩** | `splitter` `split_parallel` `merge` `compress` `pack` `decompress` `partition` `check` |
| **面向客户端** | `sheet` `book` `listing` `jobs` `cache` `demo` |
| **入口** | `crates/cli/src/main.rs`（命令行）· `serve.rs`（HTTP 后端，前端由别人接）|

## 不变式在哪守 / Where the invariants live

| 不变式 | 守住它的东西 | 测试 |
|---|---|---|
| ① 原始文件不可变 | 写入一律指向 `-o`，`check_output` 拒绝指向输入 | `no_command_modifies_the_input_file` |
| ② 内存与行数无关 | 流式读写 + 分页窗口 | 全命令 RSS 实测（`docs/CAPABILITIES.md`）|
| ③ 会骗人的地方不静默 | `report_failure` / `hint:` / `note:` / 警告 | 十余条错误契约测试 |
| 文档说真话 | 从源码提取事实再比对 | `crates/cli/tests/docs.rs` 的 13 条 |

## 改一处要看哪里 / If you change…

| 你要改 | 也请检查 |
|---|---|
| `workbook/rows.rs` 的行/列身份 | `export.rs` 的投影顺序、`journal.rs` 的格式、组合测试（改名/分列/填充 × 增删行）|
| journal 格式 | `render_op`/`parse_op` 成对改 + `tests` 里的往返测试 + 旧 journal 兼容 |
| 新增命令 | `COMMAND_OPTIONS`（帮助表）、README 命令总览、`docs/CAPABILITIES.md`、`scripts/regress.sh` —— 四条都有测试盯着 |
| 新增 API 端点/参数 | `docs/API.md`、`/sheet` 的 `capabilities`、`EDIT_ACTIONS` |
| 新增限制/上限 | 超限时**报错或明说**，不能静默截断 |
| 改动内存行为 | 用 `/usr/bin/time -l` 复测，数字写进 `CAPABILITIES.md` |

**这张表的每一行都对应一条测试。** 不是纪律，是编译器之外的检查。
