# 本地 DSH 模型接入测试

日期：2026-11-24  
限制：只测 **DeepSeek-V4.1-Flash-1017-Ark-Economy**，不接其他模型。

## 接入方式

DSH 的 `llm-pi-ai` provider 配置在 `~/.dsh/profiles/web/cordis.patch.yml`：

- provider：`hellobike`
- API：OpenAI-compatible `POST /tokenplan/v1/chat/completions`
- 模型：`DeepSeek-V4.1-Flash-1017-Ark-Economy`
- key 来自 DSH credentials，**没有写进项目，也没有打印**。

## 实测

```text
models: 239
target_present: true
completion: HTTP 200
elapsed: 1.10 s
prompt_tokens: 35
completion_tokens: 11
reasoning_tokens: 9
content: OK
```

第一次用 `max_tokens=8` 时内容为空、`completion_tokens=8` 全是 reasoning；
把上限提高到 64 后才拿到 `OK`。因此内置 Agent 的默认输出上限不能太小。

## 内置 runner 实测

```text
csvx agent run --prompt "Use csvx_schema on t.csv, then reply with the row count."
step 1: csvx_schema
step 2: csvx_view
2
real 6.10 s
```

带 `--skill skills/csvx-data/SKILL.md` 再跑一次：
`step 1: csvx_schema` → `2`，5.49 s。

再把 skill + `--allow-apply` 用于 read→preview→apply：

```text
step 1: csvx_schema
step 2: csvx_view
step 3: csvx_view
step 4: csvx_edit_preview
step 5: csvx_edit_apply
done
real 11.75 s
```

结果：基础 CSV 未改写；journal 1 条 `set row 1 city = C`；`view` 显示 `C`。

这验证了模型 → JSON 动作 → csvx 工具 → 观察回灌 → final 的闭环，
以及 skill 文本能改变 Agent 的读取路径。

## DSH 原生 skill 已装入

`bash scripts/install_skill.sh` 把仓库 skill 复制到
`$DSH_HOME/skills/csvx-data/SKILL.md`（默认 `~/.dsh`）。本机安装后，
DSH 的 skill 目录已出现 `csvx-data-workbench`，并成功通过 `skill` 工具加载。

## 尚未做

- 通过 DSH skill 调 csvx MCP；
- 更长的多轮任务与错误恢复。

## 安全

- 不在仓库保存模型 key；
- 不修改用户 DSH profile；
- 只使用用户指定的 Ark Economy 模型。
