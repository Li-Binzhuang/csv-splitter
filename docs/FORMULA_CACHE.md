# 公式/读窗口缓存

## 做法

- `workbook::window` 增加进程内缓存；
- key = 基础文件路径 + 大小 + mtime + journal ops + delimiter + start/count；
- journal 变化或文件变化会自然产生新 key，旧结果不可能被误用；
- 缓存上限 64 个窗口，超过后整体清空；
- `csvx_cache_clear` 同时清空读窗口缓存；`csvx_cache_stats` 返回 `windowHits/windowMisses`。

## 实测

100k 行 CSV + `double=amount*2` 公式 journal，release server，
`GET /rows?start=0&count=2000` 连续四次：

```text
0: 0.040 s
1: 0.007 s
2: 0.007 s
3: 0.006 s
```

首次是冷计算；后三次命中缓存，约 **5–6 倍**。

## 边界

- 只缓存窗口结果，不改变文件或 journal；
- 缓存是进程内的，server 重启后清空；
- 64 个窗口是当前上限，不是性能 SLA；
- 公式引擎本身的跨行限制不变（`BEYOND_EXCEL.md` 已写）。
