# Excel 公式覆盖矩阵

日期：2026-11-27 · 引擎：`crates/core/src/formula/` · 口径：读时逐行求值，不落盘。

状态含义：

- **支持**：解析器与 `call` 分派已有实现，且有行级测试；
- **首批补齐**：2026-11-27 新增；
- **缺失**：Excel 常用但当前会落到 `_ => Value::Error`；
- **不做**：与当前流式/显式引用架构冲突，或明确非目标。

错误语义：`#VALUE!` 是唯一内部错误值；查找未命中沿用文本 `#N/A`。
`IFERROR` 同时捕获两者，`IFNA`/`ISNA` 只捕获 `#N/A`。

## 文本

| 函数 | 状态 | 说明 |
|---|---|---|
| LEN / UPPER / LOWER / TRIM | 支持 | Unicode 字符数；TRIM 只裁首尾 |
| LEFT / RIGHT / MID | 支持 | 按字符，不按字节 |
| REPLACE / SUBSTITUTE | 支持 | SUBSTITUTE 只是 REPLACE 别名，暂不支持 instance_num |
| CONCAT / CONCATENATE | 支持 | CONCATENATE 是别名 |
| TEXTJOIN | 支持 | 分隔符 + ignore_empty + 多段 |
| CHAR / CODE | 支持 | ASCII 码点 |
| FIND / SEARCH | 支持 | SEARCH 不区分大小写；均支持起始位置 |
| PROPER / REPT / EXACT / VALUE | 支持 | VALUE 只解析数字文本 |
| TEXT | 支持（基础） | 复用 `NumberFormat`：0.00/#,##0/0%/日期时间；科学计数/会计格式仍缺 |
| TEXTBEFORE / TEXTAFTER | 支持（受限） | 支持 instance_num；match_mode/match_end/if_not_found 未实现 |
| UNICHAR / UNICODE | 缺失 | 非 ASCII 码点 |
| NUMBERVALUE / FIXED / DOLLAR | 缺失 | 区域小数点/货币格式 |
| TEXTSPLIT / ARRAYTOTEXT | 缺失 | 动态数组，非目标 |

## 日期与时间

| 函数 | 状态 | 说明 |
|---|---|---|
| DATE / YEAR / MONTH / DAY | 支持 | DATE 生成 ISO 日期文本 |
| EDATE / EOMONTH | 支持 | 月末按日历计算 |
| WEEKDAY / WEEKNUM | 支持 | 默认周日为 1；WEEKNUM 用 return_type 1 |
| DAYS / NETWORKDAYS / WORKDAY | 支持 | 工作日跳过周末；不含自定义节假日 |
| DATESTRING / DATEKEY | 支持 | 截取 ISO 前缀 |
| DATEVALUE | 支持（新增） | ISO 日期 → Excel 1900 序列号；1900-01-01=1、假闰日=60 |
| HOUR / MINUTE / SECOND | 支持（新增） | ISO 日期时间、纯时间、0–1 天小数 |
| TIMEVALUE | 支持（新增） | 时间文本 → 0–1 天小数 |
| NOW / TODAY | 支持（易变） | 读系统 UTC 时钟；不落盘，重跑可变 |
| DATEDIF | 支持（新增） | Y/M/D/YM/MD/YD；end < start 报 #VALUE! |
| YEARFRAC | 支持（受限） | basis 0=US 30/360；1=实际/365.25；2=实际/360；3=实际/365；4=欧式 30/360 |
| ISOWEEKNUM | 支持（新增） | ISO 周（周一为 1） |
| TIME | 支持（新增） | 时/分/秒 → 0–1 天小数 |

明确不做：跨时区/EOMONTH 自定义节假日；当前仓库只把日期当 ISO 前缀文本。

## 逻辑

| 函数 | 状态 | 说明 |
|---|---|---|
| IF | 支持 | 惰性分支，只求值选中的一支 |
| IFS | 首批补齐 | 条件/值成对；无命中返回 `#N/A` |
| SWITCH | 首批补齐 | 文本相等；可选默认值；无命中 `#N/A` |
| AND / OR / NOT | 支持 | 逐参短路 |
| XOR | 首批补齐 | 真值个数为奇数 |
| TRUE / FALSE | 首批补齐 | 常量函数 |
| IFERROR | 支持（修正） | 现在同时捕获 `#VALUE!` 与 `#N/A` |
| IFNA | 首批补齐 | 只捕获 `#N/A`，不吞 `#VALUE!` |
| NA | 首批补齐 | 显式 `#N/A` 常量 |
| ISBLANK / ISNUMBER / ISTEXT | 支持 | 空文本按空白处理 |
| ISERROR / ISNA | 支持 | 分别覆盖全部错误与 `#N/A` |
| LET / LAMBDA / MAP / BYROW | 不做 | 需要名字绑定与数组求值，超出逐行标量模型 |

## 查找与引用

| 函数 | 状态 | 说明 |
|---|---|---|
| XLOOKUP | 支持（受限） | 只接受 `("sheet.csv","key","value")` 显式引用，不支持数组/区间 |
| VLOOKUP / REF | 支持（受限） | 同上；未命中 `#N/A` |
| CHOOSE | 首批补齐 | 1-based 序号；越界 `#VALUE!` |
| INDEX / MATCH / XMATCH | 缺失 | 需要按行号随机访问或整列索引；当前流式只保证当前行 |
| HLOOKUP / LOOKUP | 缺失 | 同上 |
| OFFSET / INDIRECT | 不做 | 动态引用会破坏预加载与可预测性 |

## 数学与聚合

| 函数 | 状态 | 说明 |
|---|---|---|
| SUM / AVERAGE / COUNT / MIN / MAX | 支持 | 整列常量化，或同行区间 |
| COUNTIF / SUMIF / AVERAGEIF | 支持 | 同行区间 + 预扫整列 |
| COUNTIFS / SUMIFS / AVERAGEIFS / MINIFS / MAXIFS | 支持 | 同行多条件 |
| SUMPRODUCT / SUMSQ / PRODUCT | 支持 | 同行等宽区间 |
| COUNTBLANK | 支持 | 同行区间 |
| MEDIAN / MODE / LARGE / SMALL / PERCENTILE | 支持（受限） | 只读同行区间；MEDIAN 不做整列 |
| STDEV / VAR / STDEVP / VARP | 支持 | 整列 |
| ROUND / ROUNDUP / ROUNDDOWN | 支持 | 十进制位数 |
| ABS / SIGN / POWER / EXP / LN / LOG | 支持 | LOG 支持底数 |
| FLOOR / CEIL / CEILING / INT / TRUNC / SQRT / MOD | 支持 | 标量 |
| RANK | 支持（新增） | 同列范围；默认降序，order≠0 升序；并列同名次 |
| QUARTILE / QUARTILE.EXC | 支持（新增） | quart 0–4；EXC 专用边界 |
| TRIMMEAN | 支持（新增） | 按百分比两端裁剪后平均 |
| RANK.EQ / STDEV.S / VAR.S / STDEV.P / VAR.P | 支持（新增） | 点号函数名词法；同 RANK/STDEV/VAR 语义 |

| SUBTOTAL / AGGREGATE | 缺失 | 需要函数编号与忽略隐藏行语义 |
| RAND / RANDBETWEEN | 缺失 | 非确定性，暂不进入可复现导出 |
| GCD / LCM / FACT / COMBIN / PERMUT / MROUND | 缺失 | 数论/工程函数 |
| BITAND / BITOR / BITXOR | 缺失 | 位运算 |

## 本轮验收（首批补齐）

第五批新增 `rank_orders_descending_by_default_and_ascending_on_request` 与
`quartile_and_trimmean_read_same_row_ranges`。第四批测试：TIME 0.544502、ISO 周、YEARFRAC
basis 0/1、反向日期报错。第三批测试：TIMEVALUE=0.544502、
DATEDIF Y/M/D、反向日期报错、TODAY/NOW 形状。第二批测试：TEXT 数字/
百分比/日期格式、TEXTBEFORE/AFTER 第 N 个分隔符、DATEVALUE=45292、HOUR/MINUTE/
SECOND 与 0.5 天小数。第一批测试：

```text
IFS 命中/无命中；SWITCH 命中/默认/无命中；CHOOSE 正常/越界；
NA/IFNA/IFERROR 的 #N/A 语义；XOR/TRUE/FALSE。
```

命令：

```bash
cargo test -p csvx-core --lib logical_fallbacks_and_switch_are_lazy
scripts/gate.sh
```
