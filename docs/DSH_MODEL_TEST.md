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

## 尚未做

- 用该模型跑 csvx Agent API 的 read→preview→apply 回合；
- 通过 DSH skill 调 csvx MCP；
- 内置 runner 的 tool-call 回灌测试。

## 安全

- 不在仓库保存模型 key；
- 不修改用户 DSH profile；
- 只使用用户指定的 Ark Economy 模型。
