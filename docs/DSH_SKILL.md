# csvx skill 接入 DSH

## 安装

```bash
bash scripts/install_skill.sh
```

默认写入 `$DSH_HOME/skills/csvx-data/SKILL.md`（`DSH_HOME` 缺省为 `~/.dsh`）。
脚本只复制一个文件，不改 key、不改模型配置。

安装后 DSH skill 目录出现：

```text
csvx-data-workbench
```

本机已实测用 DSH 的 `skill` 工具加载成功，返回完整的
“local CSV / Excel backend” 指令。

## 与 runner 的关系

- `csvx agent run --skill skills/csvx-data/SKILL.md`：把 skill 注入模型 system prompt；
- DSH 原生 skill：由 DSH 在会话开始时按 description 路由；
- 两者共享同一份 Markdown，不维护两个版本。

## 边界

- 不安装模型、不写 API key；
- 安装脚本只覆盖 `SKILL.md`，不会删除 skill 目录里的其他文件；
- 需要重进 DSH 会话或等待 skill 扫描后才会出现在目录中。
