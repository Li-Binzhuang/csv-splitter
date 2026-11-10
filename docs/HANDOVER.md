# 交付说明 / Handover

**版本 2.0.0**，80 轮开发后的状态：这个软件是什么、能做什么、边界在哪、怎么接着做。

---

## 一句话

**一个不重写原文件的 CSV 表格引擎**：把几十 GB 的表当电子表格改，内存只跟编辑操作数
和一屏的行数有关，原始文件逐字节不变，导出时才物化。

```bash
csv-splitter demo -o .          # 生成一个真实感的示例文件
csv-splitter view orders.csv --rows 20
csv-splitter edit set orders.csv --row 3 --col city --value CDMX
csv-splitter edit formula orders.csv --name vat --value "amount*0.16"
csv-splitter edit export orders.csv -o clean.csv
```

## 对 Excel / WPS 的覆盖

| 电子表格操作 | 这里怎么做的 |
|---|---|
| 改单元格 | `edit set --row --col`（1-based，可 `--where` 批量）|
| 增删行列 | `edit insert-rows` / `delete`（按行、按条件、**按校验规则**、按内容）/ `insert-column` / `drop` / `rename` |
| 虚拟列 | `edit formula`：`lag()`、`running_total()`、A1 引用、`ref()/vlookup()` 跨表 |
| 查找替换 | `search`（含 `--regex`）、`edit replace`（文本 / 正则 / 按列 / 第 n 个）|
| 撤销重做 | `edit undo` / `edit redo`（journal 的栈，跨进程有效）|
| 排序 | `sort`（跨块归并，稳定），或 `edit export --order` |
| 筛选 | `filter --where`、`view --where`、`/rows?where=` |
| 透视表 | `pivot`（行×列×聚合，一次流式扫描）|
| 分组汇总 | `groupby`（sum/mean/min/max/count/distinct…）|
| 条件格式 | `format --rule "amount>50000:bg=red"`（服务端求值，返回可上色范围）|
| 图表数据 | `chart`（类目 + 序列，终端还能画条形图）|
| 列宽 / 冻结 / 数字格式 | `layout`（存 `.view`，与数据分离）|
| 数据校验 | `validate`（非空/数字/整数/日期/唯一/比较，日期按真实日历）|
| 分列 / 合并列 | `edit split-column --by "," --into a,b` |
| 跨表引用 | `lookup`（VLOOKUP）|
| 拼接 | `concat`（表头不一致会拒绝）|
| SQL | `sql`（DuckDB，只读，超时保护）|
| 拆分上传 | `split`（多线程、压缩、可 `verify`）|
| 给前端用 | `serve`（21 个读端点 + 11 个写动作 + 内置查看页）|

**不做的**：图形渲染（只给数据）、多 sheet 公式联动（`ref()` 是只读查找）、
真正的 xlsx/xls 读写（这是 CSV 引擎）。

## 三条承诺，各自的证据在哪

| 承诺 | 证据 |
|---|---|
| **① 原文件不可变** | 测试 `no_command_modifies_the_input_file`：哈希+大小+mtime，覆盖每条写命令，含一条故意失败的 |
| **② 内存与行数无关** | `docs/CAPABILITIES.md` 的全命令 RSS 实测表；500 万行 + 公式列导出峰值 **17.3 MB** |
| **③ 会骗人的地方不静默** | 错误契约测试、`truncated` 标记、`#VALUE!`/`#N/A`、journal 损坏警告、四种路径错误、`troubleshooting_quotes_are_real` |

**② 的准确版本**：与行数无关，**除非那个命令必须记住每一行**——`dedupe` 记键、
`sort`/`partition` 有缓冲；三者都有显式上限，超了报错。

## 性能（215 MB / 500 万行，release）

| 场景 | 时间 | 内存 |
|---|---|---|
| 建索引 | 0.08 s | 16.3 MB |
| 跳到任意行 | 0.01 s | 7.8 MB |
| 看一屏 | 0.01 s | 8.5 MB |
| 全列 analyze | 1.66 s | 17.2 MB |
| 分组聚合 | 1.39 s | 9.0 MB |
| 拆 8 片 / 4 线程 | 0.40 / 0.18 s | 90 / 120 MB |
| 合并 | 0.18 s | 13.3 MB |
| 套用编辑导出 | — | **17.3 MB** |

API：`/rows` 首屏 66 ms（含建索引），之后 **6–7 ms**，任意偏移一样快。

## 已知限制（写明，不暗示）

1. `running_total` 在窗口视图里是 `#VALUE!`（它需要起点之前的每一行）。
2. 需要读别的行的 A1 引用返回 `#VALUE!`；只有同行引用可用。
3. `sort` / `concat` / `sql` 不接受 `--quotes`。
4. `analyze`/`stats`/`search`/`sort`/`dedupe`/`concat`/`columns`/`split`/`partition`/
   `lookup` 读**文件**而非编辑后的表；`groupby`/`pivot`/`chart` 可加 `--apply-edits`。
5. `dedupe` 内存随不同键数增长；`sort`/`partition` 随 `--buf-mb` 增长。
6. 内置查看页未在真实浏览器里验证过；Windows 未测。

## 怎么接着做

| 你想做 | 先读 |
|---|---|
| 改编辑模型（行/列身份、投影顺序）| `docs/ARCHITECTURE.md` 的「编辑模型」+ `workbook/rows.rs` |
| 加一条命令 | ARCHITECTURE 的「改一处要看哪里」：帮助表、README、CAPABILITIES、`scripts/regress.sh` —— **四处都有测试盯着** |
| 加一个 API 端点 | `docs/API.md`、`/sheet` 的 capabilities、`EDIT_ACTIONS` |
| 改内存行为 | 用 `/usr/bin/time -l` 复测，数字写进 CAPABILITIES |
| 不写代码、只验证 | `scripts/dev.sh gate`（fmt+clippy+454 测试）、`smoke.sh`（43 步）、`regress.sh`（33 项）、`bench.sh` |

**日常开发节奏**（用户明确要求）：
`scripts/dev.sh check`（约 3 秒）→ 相关单测（约 5 秒）→ 攒够 1–3 个提交跑一次
`gate`（约 90 秒）→ 每轮最多推一次。**不要为一轮验证把时间耗在编译上。**

## 80 轮的教训（按价值排序）

1. **每一步都对，连起来就卡住。** 改名丢编辑、分列片被邻居覆盖、填充在删行后滑位——
   三个 bug 都是「两个正确的部分相遇」。单独测每个功能永远发现不了，所以现在有一个
   走完整旅程的测试。

2. **同一个事实写在两处，迟早会漂。** 能力清单三份、命令文档两份、路由清单两份、
   API.md 被 changelog 覆盖了 29 个版本。解法不是「记得同步」，而是**从源码提取事实再比对**。

3. **同一个承诺，两条路径实现，测试只覆盖了一条。** 「翻页常数时间」API 做到了、
   CLI 没有（深页码 5 s）；「内存与行数无关」导出路做到了、`verify` 没有（20 GB）。

4. **`str.replace` 找不到匹配时静默通过。** 五个版本号和一条 changelog 就这样丢过。
   现在每次替换都 `assert` 匹配到了。

5. **注释承诺不存在的调用者是负债。** 两个死函数各自声称「为兼容保留」「在工作线程里用」，
   两者都不存在。删代码时连注释一起删。

6. **验证轮即使没找到 bug 也不亏。** 承诺从「我们相信」变成「有测试」，这才是 2.0.0 的理由。

## 仓库结构

```
crates/core/   引擎：编辑模型、公式、查询、拆分、格式（40 个模块，19600 行）
crates/cli/    30 条命令 + HTTP API + 内置查看页
crates/gui/    egui 前端（冻结，与 core 共用，不由此项目改动）
docs/          README 之外的 7 份文档，6 条一致性测试链
scripts/       dev.sh / smoke.sh / regress.sh / bench.sh / clean.sh
```

## 一句话交接

**它现在能把几十 GB 的表当电子表格改，而且三条承诺都有测试。**
下一步最该做的不是加功能，而是拿真实数据跑一遍——**80 轮里最有效的发现方式，始终是
「真的执行那个组合」。**
