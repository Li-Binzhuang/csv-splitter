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
| REPLACE | 支持（功能级修正） | Excel 位置语义：start_num/num_chars，1-based；非法参数 `#VALUE!` |
| SUBSTITUTE | 支持（功能级修正） | 默认全替换；第 N 个 `instance_num`；越界原样，<1 `#VALUE!` |
| CONCAT / CONCATENATE | 支持 | CONCATENATE 是别名 |
| TEXTJOIN | 支持 | 分隔符 + ignore_empty + 多段 |
| CHAR / CODE / UNICHAR / UNICODE | 支持（UNICHAR/UNICODE 新增） | ASCII 与 Unicode 码点互转；非法码点/空文本报错 |
| FIND / SEARCH | 支持 | SEARCH 不区分大小写；均支持起始位置 |
| PROPER / REPT / EXACT / VALUE | 支持 | VALUE 只解析数字文本 |
| TEXT | 支持（基础） | 复用 `NumberFormat`：0.00/#,##0/0%/日期时间；科学计数/会计格式仍缺 |
| TEXTBEFORE / TEXTAFTER | 支持（受限） | 支持 instance_num；match_mode/match_end/if_not_found 未实现 |

| NUMBERVALUE / FIXED / DOLLAR | 支持（功能级新增） | FIXED 固定小数/千分位；DOLLAR 负数为括号；NUMBERVALUE 区域分隔符/百分号/括号负数 |
| TEXTSPLIT / ARRAYTOTEXT | 缺失 | 动态数组，非目标 |

## 日期与时间

| 函数 | 状态 | 说明 |
|---|---|---|
| DATE / YEAR / MONTH / DAY | 支持 | DATE 生成 ISO 日期文本 |
| EDATE / EOMONTH | 支持 | 月末按日历计算；1900-02 为 28 日（Excel 假闰日只影响序列号） |
| WEEKDAY / WEEKNUM | 支持 | 默认周日为 1；WEEKNUM 用 return_type 1 |
| DAYS / NETWORKDAYS / WORKDAY | 支持 | 工作日跳过周末；节假日支持单值或同行区间 |
| NETWORKDAYS.INTL / WORKDAY.INTL | 支持（功能级新增） | weekend 代码 1–7/11–17 或 7 位 0/1 字符串；节假日同上 |
| DAYS360 | 支持（新增） | US/EU 30/360；反向报错 |
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
| INDEX / MATCH / XMATCH | 支持（受限） | 同一行区间；INDEX 1-based 位置；MATCH 精确/近似，XMATCH 0/±1；跨行 `#VALUE!` |
| HLOOKUP / LOOKUP | 支持（受限） | HLOOKUP 同行单行表、row_index 必须 1；LOOKUP 向量形式，结果区间等宽 |
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

| SUBTOTAL | 支持（功能级新增） | 1–11/101–111；同行区间；CSV 无隐藏行，100 系列同值 |
| AGGREGATE | 支持（功能级新增） | 1–11/14–19，options 0/4/6；k 用于排名/分位；无隐藏行/错误单元格 |
| RAND / RANDBETWEEN | 缺失 | 非确定性，暂不进入可复现导出 |
| GCD / LCM / FACT / COMBIN / PERMUT | 支持（功能级新增） | 非负整数；FACT≤170；非法/非整数 `#VALUE!` |
| MROUND / CEILING.PRECISE / FLOOR.PRECISE | 支持（功能级新增） | MROUND 同号倍数；PRECISE 忽略 significance 符号；零倍数/非法参数 `#VALUE!` |
| BITAND / BITOR / BITXOR | 支持（功能级新增） | 48 位非负整数；非法/非整数/超范围 `#VALUE!` |

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
