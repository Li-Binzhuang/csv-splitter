# 前端接入 / Integrating a front-end

给接这个后端的人看的一页纸：**打开一个巨大的 CSV，把列画出来，翻行，改值，不丢人**。
每个调用都是真实存在的（`crates/cli/tests/serve.rs` 有对应测试）。

## 0. 起服务

```bash
csv-splitter serve --root /data --port 8787
# 或 --addr 127.0.0.1:8787
```

所有路径都是相对 `--root` 的；越界会被拒绝（`"path is outside the served directory"`）。

## 1. 先问它会什么（可选）

```bash
curl http://127.0.0.1:8787/capabilities
```

拿到 `reads` / `writes` / `editActions` / `limits` / `writeGuards`。
**不要把它硬编码进前端**——这份响应由服务端常量生成。

## 2. 打开文件，拿到列

```bash
curl "…/sheet?path=big.csv"      # 行数、列、是否有未应用编辑、索引状态
curl "…/columns?path=big.csv&sample=10"
```

`/columns` 返回**全部**列（782 列就 782 条）：

```json
{"count":782,"columns":[
  {"name":"base_time","index":1,"kind":"text","empty":0,"sampled":10,"distinct":64,"distinctCapped":true},
  …]}
```

- `index` 从 **1** 开始；
- `kind` 是**采样推断**（`number|date|text|empty|mixed`），用来选编辑器，不是整列保证；
- `distinct` 是采样内的不同值数，到 64 就封顶（`distinctCapped: true`）——用来判断「这列适合下拉还是自由输入」。

只要一个答案：

```bash
curl "…/columns?path=big.csv&name=cust_no"    # {"exists":true,"index":3}
```

## 3. 画一屏

```bash
curl "…/rows?path=big.csv&start=1000&count=50"
```

响应字段固定：

| 字段 | 含义 |
|---|---|
| `start` / `count` / `requested` | 实际给的起点和行数，以及你要的（超过 2000 会被截到 `limit`）|
| `total` | 逻辑行总数（含编辑）|
| `columns` / `frozenColumns` | 表头，以及冻结的前 N 列 |
| `rows` | 二维字符串数组 |
| `rowKeys` | 与 `rows` 等长：`b:<index>`（base 行）或 `v:<id>`（插入行）|
| `edited` / `operations` | 这一屏是否套用了 journal，套用了几条 |

**横向翻 782 列**：`&start-col=5&count-cols=10`（只返回这一段的字节）。
**只要几列**：`&columns=cust_no,model_prob`（顺序按你给的）。

## 4. 读一个单元格 / 改一个单元格

```bash
curl "…/cell?path=big.csv&row=3&col=predict_status"
{"row":3,"column":"predict_status","value":"success","key":"b:2","total":684917}
```

改：

```bash
# 无条件
curl -X POST "…/edit?path=big.csv&action=set&row=3&col=city&value=CDMX"

# 有条件（推荐）：只有当前值仍是期望值才写
curl -X POST "…/edit?path=big.csv&action=set&row=3&col=city&value=CDMX&expect=Puebla"
```

`expect=` 不匹配时返回 **409**，并且**什么都没写**：

```json
{"error":"the value changed; nothing was written","parameter":"expect",
 "hint":"re-read the row and retry without expect=","actual":"Puebla","expected":"CDMX"}
```

前端处理方式：**重新读那一格，再看是否还要改**。别重试同一个 `expect=`。

## 5. 行的身份

`rowKeys` 里的 `b:<n>` / `v:<id>` 是**稳定身份**。插入/删除行之后位置会变，身份不会。

**选中状态、批量操作、乐观更新都应该按 key 记，不要按「第几行」记。**

## 6. 写完之后

- 编辑进 `<file>.journal`，**原文件不动**；页面刷新后 `/rows` 仍然带编辑。
- 要让磁盘上的文件变成编辑后的样子：`POST /edit?action=export&out=clean.csv`。
- 想回退：`POST /edit?action=undo`；再改回来：`action=redo`。

## 7. 多文件

`/files?root=.` 列文件；`/book?root=.` 把一个目录当工作簿列出所有 sheet。
（本项目的「sheet」= 一个 CSV + 它的 journal。）

## 8. 不要做的假设

| 别假设 | 事实 |
|---|---|
| `rows` 长度总是等于 `count` | 文件末尾会更短 |
| `kind` 代表整列 | 只看前 `sample` 行 |
| 写入一定会成功 | `expect=` 可能 409；并发写入有锁但不是事务 |
| `/rows` 能一次取 1 万行 | 上限 2000，多取会被截并告知 `limit` |
| 编辑会改原文件 | 永远不会 |

## 9. 出错时的形状

```
400 {"error","parameter","hint"}    参数不对
404 {"error","parameter","hint"}    行/文件不存在
409 {"error","parameter","hint","actual","expected"}   expect= 不匹配
422 {"error"}                        文件读不了（编码、格式）
```

`parameter` 指明该改哪个参数，`hint` 说怎么改。**照着 hint 做就够。**
