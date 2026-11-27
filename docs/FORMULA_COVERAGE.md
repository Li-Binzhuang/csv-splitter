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
| LEN / UPPER / LOWER / TRIM | 支持（功能级扩展） | Unicode 字符数；TRIM 裁首尾并折叠内部连续空格 |
| LEFT / RIGHT / MID | 支持（功能级扩展） | 按字符；长度/起始须整数且非负（MID start≥1） |
| REPLACE | 支持（功能级修正） | Excel 位置语义：start_num/num_chars，1-based；非法参数 `#VALUE!` |
| SUBSTITUTE | 支持（功能级修正） | 默认全替换；instance_num 须整数≥1，越界原样 |
| CONCAT / CONCATENATE | 支持 | CONCATENATE 别名；结果 ≤32767 字符，超限 `#VALUE!` |
| TEXTJOIN | 支持（功能级新增） | ignore_empty 0/1；分隔符任意；结果 ≤32767 字符，超限 `#VALUE!` |
| CHAR / CODE / UNICHAR / UNICODE | 支持（边界加固） | CHAR 1–255 整数；UNICHAR 整数合法码点（0/代理区/小数报错）；CODE/UNICODE 单字符 |
| FIND / SEARCH | 支持（功能级扩展） | SEARCH Unicode 折叠 + `*`/`?`/`~` 通配符；start_num 整数≥1 |
| PROPER / REPT / EXACT / VALUE | 支持（功能级扩展） | PROPER Unicode 字母分界与大小写扩展；REPT 0–10000；VALUE 欧洲逗号小数等 |
| CLEAN | 支持（功能级新增） | 去掉 ASCII 0–31 控制字符；不裁首尾空格；多参 `#VALUE!` |
| TEXT | 支持（功能级扩展） | 复用 `NumberFormat`；支持 `正;负;零;文本` 四段与负段减号 |
| TEXTBEFORE / TEXTAFTER | 支持（功能级新增） | instance_num 须整数≥1；match_mode 0/1、match_end、if_not_found |

| NUMBERVALUE / FIXED / DOLLAR | 支持（功能级新增） | FIXED/DOLLAR 文本格式化；DOLLAR 小数位 0–4 整数；NUMBERVALUE 空文本报错 |
| TEXTSPLIT | 支持（受限，功能级新增） | 标量引擎返回首行首个非空片段；row_delimiter/ignore_empty/pad_with 生效 |

## 日期与时间

| 函数 | 状态 | 说明 |
|---|---|---|
| DATE / YEAR / MONTH / DAY | 支持 | DATE 生成 ISO 日期文本 |
| EDATE / EOMONTH | 支持 | 月末按日历计算；1900-02 为 28 日（Excel 假闰日只影响序列号） |
| WEEKDAY / WEEKNUM | 支持（功能级新增） | WEEKDAY 默认周日=1；WEEKNUM 支持 1/2/11–17/21 周起始 |
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
| TYPE | 支持（功能级新增） | 数字=1、文本=2、布尔=4、错误=16；空单元格按数字 |
| ERROR.TYPE | 支持（功能级新增） | #NULL!/#DIV/0!/#VALUE!/#REF!/#NAME?/#NUM!/#N/A → 1–7；非错误 `#N/A` |
| ISERROR / ISNA | 支持 | 分别覆盖全部错误与 `#N/A` |
| LET / LAMBDA / MAP / BYROW | 不做 | 需要名字绑定与数组求值，超出逐行标量模型 |

## 查找与引用

| 函数 | 状态 | 说明 |
|---|---|---|
| XLOOKUP | 支持（受限） | 显式引用；fallback；match_mode -1/1 近似、2 通配符 |
| VLOOKUP / REF | 支持（受限） | 显式引用；match_mode ±1 近似/2 通配符；未命中 `#N/A` |
| CHOOSE | 首批补齐 | 1-based 序号；越界 `#VALUE!` |
| INDEX / MATCH / XMATCH | 支持（受限） | 同行区间；INDEX 正整数；MATCH 0/±1；XMATCH 2 通配、search_mode -1 反向 |
| HLOOKUP / LOOKUP | 支持（受限） | HLOOKUP 单行，精确+通配符/文本近似；LOOKUP 数值/文本近似，结果等宽 |
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
| SLOPE / INTERCEPT / PEARSON / RSQ | 支持（功能级新增） | 同行等长 y/x 区间；零方差 `#VALUE!` |
| STANDARDIZE | 支持（功能级新增） | standard_dev≤0/非数值 `#VALUE!` |
| FORECAST / FORECAST.LINEAR | 支持（功能级新增） | 同行等长 y/x；零方差 `#VALUE!` |
| COVAR / COVARIANCE.P / COVARIANCE.S | 支持（功能级新增） | 同行等长 y/x；S 需至少 2 个样本 |
| CEILING.MATH / FLOOR.MATH | 支持（功能级新增） | significance 绝对值、mode 负数方向、零 significance 返回 0 |
| ISEVEN / ISODD | 支持（功能级新增） | 整数奇偶；非整数/非数值 `#VALUE!` |
| AVEDEV | 支持（功能级新增） | 同行数值区间；空区间/参数不足 `#VALUE!` |
| DEVSQ | 支持（功能级新增） | 同行数值区间；空区间 `#VALUE!` |
| GEOMEAN / HARMEAN | 支持（功能级新增） | 同行正数样本；非正数/空样本 `#VALUE!`；别名 harmmean 兼容 |
| SKEW / KURT | 支持（功能级新增） | 同行样本；SKEW≥3、KURT≥4；零方差 `#VALUE!` |
| ROUND / ROUNDUP / ROUNDDOWN | 支持 | 十进制位数 |
| ABS / SIGN / POWER / EXP / LN / LOG | 支持 | LOG 支持底数 |
| FLOOR / CEIL / CEILING / INT / TRUNC / SQRT / MOD | 支持 | 标量 |
| RANK / RANK.EQ / RANK.AVG | 支持（新增） | 默认降序，order≠0 升序；并列同名次/平均名次 |
| CHISQ.TEST | 支持（功能级新增） | 单行观测/期望等长、df=n-1 拟合检验；非法 `#VALUE!` |
| ARRAYTOTEXT | 支持（功能级新增） | 单行区间；format 0 逗号分隔、format 1 严格数组文本 |
| VALUETOTEXT | 支持（功能级新增） | format 0 简洁、1 文本加引号；错误转文本；非法 format `#VALUE!` |
| MODE / MODE.SNGL / MODE.MULT | 支持（功能级新增） | 同行区间；并列取小值；MULT 仅返回首众数（无数组结果） |
| MODE / MODE.SNGL | 支持（功能级新增） | 同行区间；并列取小值；无众数 `#VALUE!` |
| PERCENTRANK / PERCENTRANK.INC / PERCENTRANK.EXC | 支持（功能级新增） | 同行区间、插值、significance 截断；范围外 `#N/A` |
| QUARTILE / QUARTILE.INC / QUARTILE.EXC | 支持（功能级新增） | quart 0–4；INC 同 QUARTILE，EXC 专用边界 |
| QUARTILE / QUARTILE.EXC | 支持（新增） | quart 0–4；EXC 专用边界 |
| TRIMMEAN | 支持（新增） | 按百分比两端裁剪后平均 |
| RANK.EQ / STDEV.S / VAR.S / STDEV.P / VAR.P | 支持（新增） | 点号函数名词法；同 RANK/STDEV/VAR 语义 |

| SUBTOTAL | 支持（功能级新增） | 1–11/101–111；同行区间；CSV 无隐藏行，100 系列同值 |
| AGGREGATE | 支持（功能级新增） | 1–11/14–19，options 0/4/6；k 用于排名/分位；无隐藏行/错误单元格 |
| RAND / RANDBETWEEN | 缺失 | 非确定性，暂不进入可复现导出 |
| PMT | 支持（功能级新增） | rate=0/type 0/1；rate≤-1、nper=0、非法 type `#VALUE!` |
| FV | 支持（功能级新增） | rate=0/type 0/1；rate≤-1/非法 type `#VALUE!` |
| PV | 支持（功能级新增） | rate=0/type 0/1；rate≤-1/非法 type `#VALUE!` |
| NPER | 支持（功能级新增） | rate=0/type 0/1；rate≤-1/pmt=0（rate=0）`#VALUE!` |
| NPV | 支持（功能级新增） | rate≤-1/非数值/空区间 `#VALUE!`；现金流从第 1 期折现 |
| RATE | 支持（功能级新增） | Newton 迭代；nper≤0/guess≤-1/不收敛 `#VALUE!` |
| XIRR | 支持（功能级新增） | values/dates 等长、按天 Newton 求解；需正负现金流 |
| IRR | 支持（功能级新增） | 周期现金流 Newton 迭代；需正负现金流；不收敛 `#VALUE!` |
| EFFECT / NOMINAL | 支持（功能级新增） | npery 正整数；负利率/非整数周期 `#VALUE!` |
| IPMT / PPMT | 支持（功能级新增） | 期数/type/nper 边界；付款拆分为利息/本金 |
| CUMIPMT / CUMPRINC | 支持（功能级新增） | 区间累计利息/本金；start/end/type 越界 `#VALUE!` |
| SLN / SYD | 支持（功能级新增） | 直线/年数总和折旧；life≤0、period 越界/非整数 `#VALUE!` |
| DB / DDB / VDB | 支持（功能级新增） | 固定/双倍/变动余额递减；区间、月数、因子非法 `#VALUE!` |
| FVSCHEDULE | 支持（功能级新增） | 复利 schedule 区间；空区间/rate≤-1 `#VALUE!` |
| SUMXMY2 / SUMX2MY2 / SUMX2PY2 | 支持（功能级新增） | 同行等长 x/y、空区间/长度不等 `#VALUE!` |
| ISERR | 支持（功能级新增） | 除 `#N/A` 外的表达式错误返回 TRUE |
| ISLOGICAL / ISNONTEXT | 支持（功能级新增） | 布尔/文本分类；`#N/A` 文本按错误处理；无参 `#VALUE!` |
| ERF.PRECISE / ERFC.PRECISE | 支持（功能级新增） | 单参数；非有限/多参 `#VALUE!` |
| ISREF | 支持（功能级新增） | 不求得值，按表达式形状判断引用；无参 `#VALUE!` |
| HYPGEOM.INV | 支持（功能级新增） | 最小分位使累积≥alpha；alpha∈[0,1]；非法 `#VALUE!` |
| BESSELK | 支持（功能级新增） | 整数阶 0–20、x>0；数值积分+递推；非法 `#VALUE!` |
| BESSELY | 支持（功能级新增） | 整数阶 0–20、x>0；积分表示+Simpson；非法 `#VALUE!` |
| HYPGEOM.DIST | 支持（功能级新增） | 质量/累积；N/K/n/s 整数与边界校验；非法 `#VALUE!` |
| BESSELI | 支持（功能级新增） | 整数阶 0–100；幂级数；非法 `#VALUE!` |
| BESSELJ | 支持（功能级新增） | 整数阶 0–100；级数求和；非法 `#VALUE!` |
| T.TEST | 支持（功能级新增） | 成对/等方差/Welch；tails 1/2、type 1–3；非法 `#VALUE!` |
| CONFIDENCE.T | 支持（功能级新增） | alpha(0,1)、sd>0、n≥2 整数；非法 `#VALUE!` |
| CONFIDENCE.NORM | 支持（功能级新增） | alpha(0,1)、sd>0、n 正整数；非法 `#VALUE!` |
| F.TEST | 支持（功能级新增） | 双尾方差齐性检验；两区间各需≥2 值；非法 `#VALUE!` |
| Z.TEST | 支持（功能级新增） | 单行区间；省略 sigma 用样本标准差；非法 `#VALUE!` |
| GAMMA.INV | 支持（功能级新增） | p(0,1)、alpha/beta>0；非法 `#VALUE!` |
| BETA.INV | 支持（功能级新增） | p(0,1)、alpha/beta>0、A<B；可选 A/B 默认 0/1 |
| CHISQ.INV.RT | 支持（功能级新增） | 右尾反函数；概率(0,1)严格内、df>0；非法 `#VALUE!` |
| CHISQ.INV | 支持（功能级新增） | 左尾反函数；概率(0,1)严格内、df>0；非法 `#VALUE!` |
| F.INV | 支持（功能级新增） | 左尾反函数；概率(0,1)严格内、d1/d2>0 |
| F.INV.RT | 支持（功能级新增） | 右尾反函数；概率(0,1)、d1/d2>0；非法 `#VALUE!` |
| ASECH / ACSCH / ACOTH | 支持（功能级新增） | 反双曲倒数；定义域外/非法 `#VALUE!` |
| CHISQ.DIST.RT | 支持（功能级新增） | 卡方右尾；x≥0、df>0；非法 `#VALUE!` |
| NORM.INV | 支持（功能级新增） | 任意 mean、sd>0；概率(0,1)外/非法 `#VALUE!` |
| T.INV.2T | 支持（功能级新增） | 双尾反函数，返回正分位；概率(0,1)、df>0 |
| BINOM.DIST.RANGE | 支持（功能级新增） | 区间概率和；n≤170、0≤s≤t≤n；非法 `#VALUE!` |
| T.INV | 支持（功能级新增） | 左尾反函数；概率(0,1)、df>0；非法 `#VALUE!` |
| T.DIST.2T | 支持（功能级新增） | 双尾；x≥0、df>0；非法 `#VALUE!` |
| BINOM.INV | 支持（功能级新增） | 最小 k 使累积≥alpha；n≤170、p/alpha∈[0,1] |
| T.DIST.RT | 支持（功能级新增） | t 右尾；x≥0、df>0；非法 `#VALUE!` |
| F.DIST.RT | 支持（功能级新增） | F 右尾；x≥0、d1/d2>0；非法 `#VALUE!` |
| LOGNORM.INV | 支持（功能级新增） | p(0,1)、sd>0；非法 `#VALUE!` |
| T.DIST | 支持（功能级新增） | 任意 x、df>0；密度/左尾；非法 `#VALUE!` |
| F.DIST | 支持（功能级新增） | x≥0、d1/d2>0；密度/累积；非法 `#VALUE!` |
| NORM.DIST | 支持（功能级新增） | 任意 mean/sd>0；密度/累积；非法 `#VALUE!` |
| CHISQ.DIST | 支持（功能级新增） | x≥0、df>0；密度/累积；非法 `#VALUE!` |
| GAMMA.DIST | 支持（功能级新增） | x≥0、alpha/beta>0；密度/累积；非法 `#VALUE!` |
| BETA.DIST | 支持（功能级新增） | x∈[0,1]、alpha/beta>0；密度/累积；非法 `#VALUE!` |
| WEIBULL.DIST | 支持（功能级新增） | x≥0、alpha/beta>0；密度/累积；非法 `#VALUE!` |
| LOGNORM.DIST | 支持（功能级新增） | x>0、sd>0；密度/累积；非法 `#VALUE!` |
| BINOM.DIST | 支持（功能级新增） | k≤n≤170、p∈[0,1]；质量/累积；非法 `#VALUE!` |
| EXPON.DIST | 支持（功能级新增） | x≥0、lambda>0；密度/累积；非法 `#VALUE!` |
| POISSON.DIST | 支持（功能级新增） | k≤170、mean>0；质量/累积；非法 `#VALUE!` |
| NORM.S.DIST | 支持（功能级新增） | cumulative/密度；非数值 `#VALUE!` |
| PHI | 支持（功能级新增） | 标准正态密度；非数值 `#VALUE!` |
| NORM.S.INV | 支持（功能级新增） | 标准正态分位；概率(0,1)外 `#VALUE!` |
| N / T | 支持（功能级新增） | 数字/文本/布尔强制转换；错误透传 |
| GAUSS | 支持（功能级新增） | 标准正态累积概率减 0.5；非数值 `#VALUE!` |
| SEC / CSC | 支持（功能级新增） | 正割/余割；零点/非数值 `#VALUE!` |
| PERMUTATIONA | 支持（功能级新增） | `n^k`；非负整数、非有限结果 `#VALUE!` |
| SQRTPI | 支持（功能级新增） | `sqrt(number*pi)`；负数/非数值 `#VALUE!` |
| IMCOT | 支持（功能级新增） | 复数余切；零点/非法复数 `#VALUE!` |
| IMCSC / IMSEC | 支持（功能级新增） | 复数余割/正割；零点/非法复数 `#VALUE!` |
| IMSIN / IMCOS / IMTAN | 支持（功能级新增） | 复数三角；非法复数 `#VALUE!` |
| CSCH / SECH / COTH | 支持（功能级新增） | 双曲倒数；零点/非数值 `#VALUE!` |
| IMCOSH / IMSINH | 支持（功能级新增） | 复数双曲余弦/正弦；非法复数 `#VALUE!` |
| DEGREES / RADIANS | 支持（功能级新增） | 角度/弧度互转；非数值 `#VALUE!` |
| ISO.CEILING | 支持（功能级新增） | 绝对 significance、朝 +∞ 取整；零 significance 返回 0 |
| ASINH / ACOSH / ATANH | 支持（功能级新增） | 反双曲函数；定义域外 `#VALUE!` |
| SINH / COSH / TANH | 支持（功能级新增） | 双曲函数；非有限结果 `#VALUE!` |
| COT / ACOT | 支持（功能级新增） | 余切/反余切；零点/非数值 `#VALUE!` |
| COMBINA | 支持（功能级新增） | 可重复组合；非负整数、总和≤170；非法 `#VALUE!` |
| GAMMA | 支持（功能级新增） | 正数域；非正/非有限 `#VALUE!` |
| ERF / ERFC | 支持（功能级新增） | 误差函数近似；ERF 支持上下限两参数 |
| GESTEP | 支持（功能级新增） | 大于等于阈值返回 1；非数值/参数过多 `#VALUE!` |
| DELTA | 支持（功能级新增） | 两数相等返回 1；非数值/参数过多 `#VALUE!` |
| IMLOG2 | 支持（功能级新增） | 二进制对数；零/非法复数 `#VALUE!` |
| IMLN | 支持（功能级新增） | 自然对数；零/非法复数 `#VALUE!` |
| IMPOWER | 支持（功能级新增） | 实幂；零的负幂 `#VALUE!` |
| IMSQRT | 支持（功能级新增） | 主平方根；零复数返回 0 |
| IMEXP / IMLOG10 | 支持（功能级新增） | 复数指数/常用对数；零复数 log `#VALUE!` |
| IMCONJUGATE / IMARGUMENT | 支持（功能级新增） | 共轭/极角；零复数 argument `#VALUE!` |
| IMPRODUCT / IMDIV | 支持（功能级新增） | 两个复数文本；除零/非法复数 `#VALUE!` |
| IMSUM / IMSUB | 支持（功能级新增） | 两个复数文本；保留首个后缀；非法复数 `#VALUE!` |
| STEYX | 支持（功能级新增） | 同行等长 y/x、样本≥3、零方差 `#VALUE!` |
| GAMMALN / GAMMALN.PRECISE | 支持（功能级新增） | 正数域；非正/非有限 `#VALUE!` |
| COMPLEX / IMREAL / IMAGINARY / IMABS | 支持（功能级新增） | i/j 后缀；非法复数 `#VALUE!` |
| MULTINOMIAL | 支持（功能级新增） | 同行非负整数、总和≤170；空区间/负数 `#VALUE!` |
| FACTDOUBLE | 支持（功能级新增） | 非负整数 0..300；负数/非整数/过大 `#VALUE!` |
| ROMAN / ARABIC | 支持（功能级新增） | 0..3999；form 须整数 0..4；ARABIC 大小写不敏感，非法罗马数字报错 |
| ENCODEURL | 支持（功能级新增） | UTF-8 百分号编码；RFC3986 unreserved 保留 |
| DOLLARDE / DOLLARFR | 支持（功能级新增） | 分数分母正整数；非正/非整数 `#VALUE!` |
| MAXA / MINA | 支持（功能级新增） | 文本按 0、TRUE/FALSE 按 1/0；空样本 `#VALUE!` |
| AVERAGEA | 支持（功能级新增） | 文本按 0、TRUE/FALSE 按 1/0；空样本 `#VALUE!` |
| STDEVPA / VARPA | 支持（功能级新增） | 文本按 0、TRUE/FALSE 按 1/0；空样本 `#VALUE!` |
| STDEVA / VARA | 支持（功能级新增） | 文本按 0、TRUE/FALSE 按 1/0；样本不足 `#VALUE!` |
| SERIESSUM | 支持（功能级新增） | 系数同行区间；空区间/非数值 `#VALUE!` |
| FISHER / FISHERINV | 支持（功能级新增） | x 必须 (-1,1)；非有限值 `#VALUE!` |
| MIRR | 支持（功能级新增） | 正负现金流、finance/reinvest >-1；不合法 `#VALUE!` |
| PDURATION | 支持（功能级新增） | 正利率/正 pv/fv；零利率仅 pv=fv 返回 0 |
| RRI | 支持（功能级新增） | nper≤0、pv=0、异号现金流 `#VALUE!` |
| XNPV | 支持（功能级新增） | 按实际天数/365 折现；values/dates 等长；非法日期/利率 `#VALUE!` |
| GCD / LCM / FACT / COMBIN / PERMUT | 支持（功能级新增） | 非负整数；FACT≤170；非法/非整数 `#VALUE!` |
| MROUND / CEILING.PRECISE / FLOOR.PRECISE | 支持（功能级新增） | MROUND 同号倍数；PRECISE 忽略 significance 符号；零倍数/非法参数 `#VALUE!` |
| DECIMAL / BASE | 支持（功能级新增） | 2–36 进制；DECIMAL 非法字符/仅符号报错，BASE 非负整数/min_length 0–255 |
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

第一百四十九批新增 BESSELY：`bessely_returns_second_kind_values`（1/2/0.5 与 0 阶、边界错误）。

命令：

```bash
cargo test -p csvx-core --lib logical_fallbacks_and_switch_are_lazy
scripts/gate.sh
```
