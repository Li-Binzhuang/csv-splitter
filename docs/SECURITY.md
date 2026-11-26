# 安全评审材料（本地部署）

状态：**本地单机工具；没有托管服务、遥测或多用户鉴权。**
本文只列已有代码与实测证据，不承诺未验证的能力。

## 1. 数据边界

- 所有数据留在本机；CLI 与 MCP 不发起网络请求。
- HTTP Agent API 默认绑定 `127.0.0.1:8788`，只供本机 agent 使用。
- MCP root 是显式目录边界；工具参数中的路径必须在 root 内。
- 发布包安装脚本只读本地 tar.gz，不访问 registry 或网络。

## 2. 认证与授权

- HTTP 会话创建返回一次性 token；后续 `/agent/v1/*` 请求必须带 token。
- 实测：`scripts/agent_routes_smoke.py` 的 `auth_guard=401`、`unauth=12`；
  token 错误返回 401，未知路由返回 404。
- MCP stdio 没有网络监听面；由客户端进程启动，继承其本地权限。

## 3. 文件与编辑安全

- MCP 路径检查拒绝 `..` 与绝对路径逃逸：
  `scripts/mcp_sdk/client.mjs` 实测 `root_escape_rejections=2`。
- 编辑通过 journal + undo；原文件不直接覆盖，失败写入有回滚。
- Pipeline 失败不写状态、不留 partial 输出；staging 清理与失败路径有
  `crates/cli/tests/cli.rs` 回归测试。
- 离线安装器校验 SHA256 后才解包；坏 SHA256 拒绝且不写 manifest：
  `scripts/test_install_offline.sh` 实测 `bad_sha=rejected`。

## 4. MCP 边界

- 23 个工具、2 个资源（`csvx://agent`、`csvx://operations`）之外的请求返回错误。
- `resources=3` 包含审计资源；`csvx://audit` 只读本地 JSONL。
- 官方 SDK 回归实测：
  `sdk_calls=23`、`required_rejections=20`、`bool_rejections=2`、
  `range_rejections=8`、`root_escape_rejections=2`。
- 工具 schema 结构与 annotations 由 `scripts/check_mcp_schema.py` 和
  `scripts/check_mcp_sdk.sh` 在 gate 中检查。

## 5. 审计

- MCP 每次 `call_tool` 写 `.csvx/mcp-audit.jsonl`；
  参数只记录 key/type/长度，不落原始值。
- 默认 4 MiB 轮转，保留 `.jsonl.1`；写入失败只报 stderr，不影响工具结果。
- 实测：`audit_entries=63`、`audit_redacted=true`；
  单测覆盖脱敏、轮转、不可写路径。
- 查询：`csvx mcp audit --tool ... --errors --limit ... --json`。

## 6. 遥测与外部依赖

- 代码不包含遥测端点或自动上传路径。
- 发布包验证、离线安装、MCP/schema 回归均在本地执行。
- HTTP agent 的模型调用由用户显式启动 `csvx agent run` 时才会发生；
  本机 `agent serve` 本身不调用外部模型。

## 7. 已知限制

- 没有多用户鉴权、角色或租户隔离；HTTP token 只保护本机会话。
- 没有操作系统级沙箱；root 边界是应用层路径校验，不是容器隔离。
- 审计日志是本机 JSONL，没有集中收集、签名或防篡改存储。
- 发布包校验依赖本地 `.sha256` 与 manifest；没有在线签名/证书链。

## 8. 评审复核命令

```bash
bash scripts/gate.sh
bash scripts/check_mcp_sdk.sh
python3 scripts/check_mcp_schema.py
bash scripts/test_install_offline.sh
bash scripts/verify_dist.sh dist/csvx-*.tar.gz
```

以上命令通过只证明对应实测路径；不是对整个部署环境的安全认证。
