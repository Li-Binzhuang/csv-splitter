# 能力清单 / Capabilities

一页看完这个引擎能做什么、不能做什么、以及每个能力的边界在哪里。
命令行和 HTTP API 是同一套能力的两个入口。

## 顶层概念

| 概念 | 是什么 |
|---|---|
| **base** | 原始 CSV 文件，**永不修改** |
| **journal** | `<file>.journal`，追加式操作日志，记「单元格是什么」 |
| **view** | `<file>.view`，列宽/显示名/冻结/数字格式，记「单元格长什么样」 |
| **index** | `<file>.idx`，稀疏行索引，让跳转到任意行是 O(1) |
| **sheet** | base + journal 呈现出来的一张表 |
| **book** | 一个目录下所有 CSV 组成的工作簿 |

三条不变量：

1. **原始文件不可变**——所有编辑都在日志里，导出才物化。
2. **成本跟着窗口/操作数走，不跟行数走**——一屏 100 行只读 100 行。
3. **会骗人的地方不静默**——缺失写 `#N/A`，非数字写 `#VALUE!`，忽略编辑会明说。

## 读取与导航

| 能力 | CLI | API |
|---|---|---|
| 预览（可跳任意行） | `preview f.csv --rows 20 [--start-row N]` | `GET /rows?path=&start=&count=` |
| 建索引 | `index f.csv` | 自动（首次跳转时） |
| 文件元信息 | — | `GET /meta?path=` |
| 工作簿列表 | `book <dir>` | `GET /book` |
| 表结构 | `preview` / `analyze` | `GET /sheet` |
| 列统计（采样） | `stats f.csv` | `GET /stats?path=` |
| 全列画像 | `analyze f.csv [--sample N]` | `GET /analyze?path=` |

## 编码

| 支持 | 说明 |
|---|---|
| UTF-8 / UTF-8 BOM | 原生 |
| GBK / GB18030 | 探测（能干净解码且含 CJK 才认） |
| UTF-16 LE/BE | 读取；索引拒绝（字节偏移不可靠） |
| Windows-1252 | 与 GBK 打分竞争，`café` 不乱码 |
| 转码 | `convert f.csv -o utf8.csv`（流式，恒定内存） |
| 引号风格 | `--quotes rfc4180\|backslash\|none`，**所有读 CSV 的命令**都接受，并自动检测警告 |

## 分析

| 能力 | CLI | API | 边界 |
|---|---|---|---|
| 分组聚合 | `groupby --by K --agg c:op` | `GET /groupby` | `--max-groups` 默认 100 万 |
| 透视表 | `pivot --rows R --cols C --values c:op` | `GET /pivot` | `--max-columns` 512 / `--max-cells` 200 万 |
| 图表数据 | `chart --x C --y c:op [--series S]` | `GET /chart` | `--max-points` 5000 |
| SQL | `sql f.csv "SELECT ..."` | `GET /query`、`/rows?sql=` | DuckDB，只读单语句，结果封顶 |
| 筛选 | `filter --where "c=v"` | `GET /filter`、`/rows?where=` | 支持 `= != > >= < <= ~ in empty` |
| 搜索 | `search f.csv "text"` | `GET /search` | 返回行号，可配合跳转 |
| 跨表查找 | `lookup --key C --from R --get cols` | — | `--max-keys` 500 万 |

聚合的算子统一是 `count / sum / mean / min / max`；`sum`/`mean` 用 Kahan 补偿求和。

## 编辑（不重写原文件）

| 能力 | CLI | API |
|---|---|---|
| 改单元格 | `edit set --row N --col C --value V` | `POST /edit?action=set` |
| 插入行 | `edit insert --row N --value "a,b,c"` | `action=insert` |
| 删除行 | `edit delete --row N --count K` | `action=delete` |
| 分页浏览（套用编辑） | `view f.csv [--where EXPR] [--order COL[--desc]] [--start N] [--count M] [--json]` | `GET /rows?where=` |
| 查找替换 | `edit replace --from X --to Y [--col C]` | `action=replace` |
| 删列/改名 | `edit drop --name C` / `edit rename` | `action=drop/rename` |
| 公式列 | `edit formula --name N --value "EXPR"` | — |
| 撤销 | `edit undo` | `POST /edit?action=undo` |
| 查看待生效 | `edit status` | `GET /edit` |
| 物化 | `edit export -o out.csv` | `POST /edit?action=export` |

编辑是**粘在行上**的：先改第 50 万行、再在第 7 行插一行，那条改动会跟着原行走。

### 公式

```
算术 + - * / %  ()      比较 = <> > >= < <=
逻辑 if(a,b,c) and or not isblank isnumber iferror
文本 & "字符串" left right mid trim replace concat upper lower len
数字 round abs floor ceil int sqrt mod min max
日期 year(d) month(d) day(d)     ── 取 ISO 前缀（2024-03-15，带时间也行）
列引用 amount   [客户编号]
```

`if` 只算选中的分支（`if(qty=0, 0, amount/qty)` 不会除零）。
空单元格当 0；非数字得 `#VALUE!`；引用不存在的列 → **导出前报错**。

## 格式化与呈现

| 能力 | CLI | API |
|---|---|---|
| 条件格式 | `format --rule "amount>1000:bg=red" [--summary]` | `GET /style?rules=...` |
| 列宽/显示名/冻结 | `layout --set "c:width=160"` | `GET /layout` |
| 数字格式 | `--set "c:format=#,##0.00"` | 同上 |
| 内置浏览页面 | — | `GET /` |

条件格式的规则语法：`列<运算符>值:样式`，运算符 `> >= < <= = != ~ ^ $ matches between empty nonempty`，
样式 `bg= fg= bold italic`（颜色用名字或 `#rgb`/`#rrggbb`）。**后面的规则赢**。

## 交付

| 能力 | CLI | API |
|---|---|---|
| 切分 | `split f.csv -n N` / `--by-size` | `POST /split` |
| 合并 | `merge -i part*.tar -o out.csv` | `POST /merge` |
| 排序 | `sort --by C -o out.csv` | `POST /sort` |
| 去重 | `dedupe -o out.csv [--by C]` | `POST /dedupe` |
| 分文件 | `partition --by C -o parts.tar` | `POST /partition` |
| 拼接 | `concat a.csv b.csv ...` | — |
| 导列/改名 | `columns --keep/--drop/--rename` | — |
| 下载/导出 | — | `GET /download`、`/export` |

切分/合并是**字节级无损**（`verify` 逐字节比对）；其他写操作支持 `--verify` 计数级自检。

## 内存与边界（实测）

| 操作 | 内存 | 说明 |
|---|---|---|
| 跳转任意行（9.9 GB） | 11.3 MB | 稀疏索引 |
| `analyze` / `preview`（322 MB） | 17.1 / 8.3 MB | 2026-11-10 复测 |
| 全列 analyze（1 GB） | 29 MB | 流式 |
| 切分（9.9 GB） | 89 MB | 输出缓冲 |
| 合并（10.7 GB） | 10.7 MB | 流式合并 |
| 编辑 100 万行 | 60 字节日志 | 只有操作数占空间 |
| 透视 100 万行 | 矩阵大小 | 5×12 = 60 个累加器 |

## 明确的非目标

- 不做公式图表/透视表的**图形渲染**（只提供数据，前端画）。
- 不做多人协作、不做事务。
- 不修改原文件、不产生隐式备份。
- 不做 xlsx 读写。

## 已知短板

| 短板 | 影响 |
|---|---|
| 内置页面没在真浏览器验证过 | 有契约测试兜底，布局未经人眼 |
| `analyze` 全表扫描（1 GB 约 11 s） | 用 `--sample N` 可以快 5 倍，但那只是**前 N 行的估算** |
| Windows 未实测 | 路径处理可能有坑 |
| 多 sheet 工作簿只有列表，没有跨表引用 | 还不能 `=customers!B2` |

## 大文件拆分 / Module layout

| 文件 | 行数 | 内容 |
|---|---|---|
| `crates/core/src/workbook/mod.rs` | ~1075 | `Op`、`Sheet`、列布局、公开接口 |
| `crates/core/src/workbook/rows.rs` | ~832 | 行身份、逻辑↔物理映射、单元格编辑、公式编译 |
| `crates/core/src/workbook/journal.rs` | ~346 | journal 结构、文本格式、撤销/重做栈 |
| `crates/core/src/workbook/export.rs` | ~379 | 导出、窗口读取、行装配 |

`workbook.rs` 曾经超过 2500 行。拆分的判据不是行数本身，而是**改动的边界**：
导出和窗口读取会接触每一行，是最常变的部分；journal 的文本格式几乎不变。

## 缓存与失效 / Caching

服务端有两处缓存，都按同一个 `Stamp` 判断是否过期：

| 缓存 | 存什么 | 失效条件 |
|---|---|---|
| pager 缓存 | 打开的 sheet（索引、列布局、公式） | journal 内容哈希、基础文件大小/时间戳、分隔符任一变化 |
| 物化缓存 | 编辑后表的临时副本（给 `where=`/`order=` 用） | journal 内容哈希变化 |

**journal 用内容哈希而不是时间戳**：同一次时间戳内写两次，`(mtime, len)` 不变，
缓存会返回上一版——这在上线前真的发生过一次。基础文件不哈希（可能 10 GB），
它本来就该不可变。

实测（20 万行、1 条编辑）：首个请求 41 ms，之后任意页 **6 ms**（含第 15 万行）。
