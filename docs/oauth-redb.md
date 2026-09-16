# OAuth 客户端与 redb 持久化

依据：Claude Code **2.1.272** 的本地二进制分析。以下是对其内嵌 JavaScript 中 OAuth 行为的协议归纳，不是供应商承诺稳定的公共 API；CLI 升级时应复核。SDK–CLI 控制请求见 [协议文档](sdk-cli-control-requests.md)。

## 数据流

```text
React ── Admin Bearer ──> Axum 管理 API
       <── 官方授权 URL ── Rust 生成 state/verifier + PKCE S256
浏览器 ── 用户授权 ──> Claude 官方页面 ──> code#state
React ── code#state ──> Rust 校验 state ──> 官方 token endpoint
                                      └─> redb 事务保存

Messages 调用方 ── 网关 API key ──> Axum
                                  ├─ redb 读取 / 必要时刷新并写回
                                  └─ CLI 临时环境：access token / API key
                                     └─ stdio SDK 控制协议 ──> 模型
```

## 官方 OAuth 请求

公开客户端 ID：`9d1c250a-e61b-44d9-88ed-5944d1962f5e`。不使用 client secret。

| 用途 | 端点 |
|---|---|
| Claude 订阅授权 | `https://claude.com/cai/oauth/authorize` |
| Console 授权 | `https://platform.claude.com/oauth/authorize` |
| 手动回调 | `https://platform.claude.com/oauth/code/callback` |
| 交换 / 刷新 token | `https://platform.claude.com/v1/oauth/token` |
| 账号资料 | `https://api.anthropic.com/api/oauth/profile` |
| Console API key | `https://api.anthropic.com/api/oauth/claude_cli/create_api_key` |

授权 GET 参数：`code=true`、`response_type=code`、`client_id`、`redirect_uri`、`state`、`code_challenge`、`code_challenge_method=S256` 和 `scope`。`code_challenge` 是 verifier 的 SHA-256 摘要经 base64url 无填充编码。可选 `login_hint` 为邮箱，企业登录使用 `login_method=sso`。

请求的 scope 与该版本 CLI 一致：

```text
org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload
```

授权码交换使用 POST JSON：

```json
{
  "grant_type": "authorization_code",
  "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
  "code": "<授权码，不含 #state>",
  "state": "<服务端保存的随机 state>",
  "code_verifier": "<只存在服务端内存的 verifier>",
  "redirect_uri": "https://platform.claude.com/oauth/code/callback"
}
```

读取响应中的 `access_token`、`refresh_token`、`expires_in`、`scope` 以及可选 `account`、`organization`。资料请求使用 `Authorization: Bearer <access_token>`；资料请求失败不阻止保存有效授权。

含 `user:inference` 时使用 OAuth access token 推理。Console 授权如果没有该 scope、但具有 `org:create_api_key`，则向 Console key 端点 POST JSON `null`，使用 Bearer access token，从响应 `raw_key` 保存 API key。两种方式的凭据都留在 redb。

刷新使用 POST JSON：`grant_type=refresh_token`、`refresh_token`、`client_id`、已授予 scopes 拼接的 `scope`。当前实现于推理前五分钟刷新，不启动后台定时任务；单进程互斥避免多个请求重复使用已轮换的 refresh token。新响应省略 refresh token 时保留旧值。

OAuth HTTP 客户端禁用重定向、设置 30 秒超时、限制响应体 256 KiB；错误只暴露 HTTP 状态，不回传供应商响应体或 token 请求内容。运行时不接受浏览器指定 OAuth endpoint。

## 会话与存储

授权会话状态为 `waiting → submitting → succeeded / failed`，也可进入 `cancelled / expired`。授权会话只在内存中，重启后需要重新开始尚未完成的授权。state 必须匹配，完整授权码只接受一次；取消或失败时保留现有账号。

redb 表 `oauth_credentials_v1`：键为 `active`，值为序列化的 Credential 字节。保存字段：`access_token`、`refresh_token`、`expires_at`（Unix 秒）、`scopes`、可选 `api_key`、`method`、`email`、`organization`、`subscription`。JSON 是数据库值的编码方式，没有单独写出 JSON 凭据文件。

数据库独占打开，文件权限 `0600`，写入/删除提交事务；重启可恢复。当前只保留一个账号，不提供账号池、导出 token 或远程数据库接口。状态接口从白名单字段构造响应，账号凭据不返回浏览器。

redb 没有额外静态加密。退出删除 active 记录，不保证底层空闲页覆盖，不撤销供应商授权。需要整体保护时应使用加密磁盘和受控备份。管理令牌、网关 API key 通过 `.env` / 进程环境配置，与上游 OAuth 凭据分开。

CLI 的 `CLAUDE_CONFIG_DIR` 和 `CLAUDE_SECURESTORAGE_CONFIG_DIR` 指向每个请求的临时目录，旧的上游认证环境变量先清除。OAuth 模式只注入 `CLAUDE_CODE_OAUTH_TOKEN`，Console key 模式注入 `ANTHROPIC_API_KEY`；refresh token、管理令牌、网关密钥不传入 CLI。请求结束回收子进程及临时目录。

## 证据定位与验证

该二进制字节偏移约 167203173–167206600 包含 OAuth 配置；169805059–169807000 附近包含授权链接、授权码交换和刷新；169803166 附近包含资料请求；169810050 附近包含 Console key 请求；184661981 附近包含 PKCE 与手动回调处理。偏移仅用于定位这一版本，不能套用于其他二进制。

`tests/admin_oauth.rs` 使用本地 OAuth HTTP fixture 检验 PKCE、state、刷新轮换、redb 恢复与退出、Console key 和错误脱敏；`tests/real_oauth_cli.rs` 用真实 CLI 对接本地假推理 API，验证 redb access token 注入。测试没有使用真实账号或调用云端模型。

背景参考：[Claude Code 身份认证](https://code.claude.com/docs/en/authentication)、[CLI auth 命令](https://code.claude.com/docs/en/cli-reference)。上述内部端点细节来自本地二进制分析，而非这两份公开文档的接口保证。
