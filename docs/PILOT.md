# 试点报告生成器

对一份客户样本自动生成可复现的本地报告；不联网、不读数据内容。

```bash
CSVX_PILOT_FILE=/path/to/sample.csv \
CSVX_PILOT_WHERE=city=CDMX \
CSVX_PILOT_KEY=id CSVX_PILOT_GROUP=city \
python3 scripts/pilot_report.py
```

输出文件默认 `pilot-report.md`，可用 `CSVX_PILOT_OUT` 改路径。

## 输出
- 样本大小、行数、列数；
- filter 扫描/物化导出、search 全扫、quality 的墙钟时间；
- 子进程近似峰值 RSS；
- 明确说明“单次本地实测、非 SLA、未 drop cache”。

## 边界
- 默认会执行物化导出；大样本先设 `CSVX_PILOT_SKIP_EXPORT=1`。
- 压缩输入可用，但报告要标注压缩比/CPU 受限。
- 报告不含数据内容、遥测；分享前仍应确认样本授权。

## 适合
销售试点、客户 POC、内部对比；与 `docs/BENCHMARKS.md` 的实验室数字分开引用。
