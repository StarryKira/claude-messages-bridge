# CLI OAuth RPC 与 redb 持久化

依据：Claude Code **2.1.272** 的原生 stdio 控制处理器和真实 CLI 联调。SDK 控制请求总表见 [协议文档](sdk-cli-control-requests.md)。这些内部接口可能随 CLI 版本变化。

## 请求边界

```text
React ── Admin Bearer ──> Axum
                         └─ stdio control_request ──> Claude Code CLI
                                                      ├─ 官方 OAuth / profile
                                                      ├─ token 刷新
                                                      ├─ Console API key
                                                      └─ Messages / CLI 遥测

redb <── 保存 CLI 更新 ── 私有临时认证缓存 ── CLI 原生凭据存储
      ── 启动时恢复 ───>
```

Rust 没有发往 Anthropic 的 HTTP 客户端，不构造 token 请求，不生成 PKCE，不固定 OAuth client ID 或 scope。浏览器打开的官方授权链接也由 CLI 返回。

## 1. 发起登录

同一个 CLI 进程先完成 `initialize`，随后发送：

```json
{"type":"control_request","request_id":"auth-1","request":{"subtype":"claude_authenticate","loginWithClaudeAi":true}}
```

`true` 为 Claude 订阅，`false` 为 Console。成功响应的业务 payload：

```json
{"manualUrl":"https://claude.com/cai/oauth/authorize?...","automaticUrl":"https://claude.com/cai/oauth/authorize?..."}
```

CLI 负责随机 state、PKCE verifier/challenge、client ID、scope、回调监听器与官方 URL。前端只使用 `manualUrl`；服务校验返回的 HTTPS 域名并提取 state，以校验后续粘贴的 `code#state`。CLI RPC 不支持邮箱提示或 SSO 参数，桥接器拒绝这些额外字段；用户在官方页面选择登录方式。

## 2. 提交授权码

在启动该授权的同一个 CLI 进程中发送：

```json
{"type":"control_request","request_id":"auth-2","request":{"subtype":"claude_oauth_callback","authorizationCode":"<code>","state":"<state>"}}
```

本地接口先验证完整 `code#state`，随后只把 code 和 state 字段传给 CLI。CLI 内部使用其保存的 verifier 和 state 交换 token、查询 profile、处理 Console API key、检查组织策略并保存凭据。成功 payload 包含 `account.email`、`organization`、`subscriptionType`、`tokenSource`、`apiKeySource`、`apiProvider` 等元数据，不包含 token。

`claude_oauth_callback` 本身等待登录 flow 完成。不能在它成功后再调用 `claude_oauth_wait_for_completion`，因为 CLI 可能已经清理该 flow；后者用于开始登录后等待浏览器自动回调的流程：

```json
{"type":"control_request","request_id":"auth-3","request":{"subtype":"claude_oauth_wait_for_completion"}}
```

本控制台采用手动回调，所以仅发送前两种认证 RPC。取消或超时关闭对应 CLI 进程；CLI 退出后清理未完成授权的临时目录，不替换现有账号。

## 3. 凭据与自动刷新

CLI 的认证 RPC 不直接返回完整 access/refresh token，因此持久化采用本地原生存储适配，不增加 HTTP 请求：

1. 登录 CLI 使用独立私有缓存，`CLAUDE_CONFIG_DIR` 和 `CLAUDE_SECURESTORAGE_CONFIG_DIR` 都指向该目录。
2. CLI 完成原生登录、保存凭据并返回 RPC 成功应答后，桥接器读取缓存并提交 redb 事务。
3. 推理 CLI 从当前账号缓存读取完整原生凭据，按自身逻辑刷新 token。桥接器没有自定义刷新 RPC 或 Rust 刷新 HTTP 实现。
4. 初始化后、完整模型响应发送前及请求清理时，将最新凭据写回 redb。并发 CLI 共用认证缓存，以复用 CLI 的跨进程刷新锁与 CAS 更新。
5. 退出账号清除 redb 和缓存。此操作只涉及本地存储，不撤销供应商侧授权。

macOS 通过私有 PATH 下的 `security` 兼容脚本，把原生凭据服务映射为缓存内的 `.keychain-credentials.json` / `.console-key`。脚本仅接受与当前目录 hash 对应的服务名称，不调用真正的系统 keychain，不访问网络。CLI 的 `.credentials.json` 文件回退与模拟 keychain 分开，避免原生代码在迁移到主存储后删除回退文件时删除同一份数据。Linux 直接使用 CLI 的原生文件存储。

临时目录为 `0700`，凭据文件为 `0600`。正常退出清理缓存，崩溃可能遗留临时文件或尚未同步的刷新结果；不能宣称运行期间凭据从不写入临时 JSON。redb 为持久化来源，文件本身没有额外加密。

## 4. redb 兼容与公开状态

沿用 `oauth_credentials_v1` 表和 `active` 键。旧记录的 access/refresh token、过期时间、scope、API key 与元数据仍可读取；新增 `native_credentials`、`native_config` 保留 CLI 原生的 `clientId`、`refreshTokenExpiresAt`、订阅/限额字段及必要账号信息，避免刷新时丢失 CLI 需要的字段。旧记录缺少新字段时自动构造兼容缓存，无需重新登录才能迁移。

管理状态只返回账号元数据，token 不返回前端。CLI stderr 和控制应答中的错误内容不直接透传到浏览器，避免带出凭据。管理令牌和 Messages key 不传入 CLI。

## 验证

- `tests/admin_oauth.rs`：原生 RPC 顺序、state、单次提交、取消、超时、错误脱敏、redb 恢复、CLI 更新写回、退出清理。
- `tests/real_auth_rpc_smoke.py`：真实 CLI，通过只响应本地数据的 HTTPS 代理验证 OAuth RPC、PKCE、profile、Console key、刷新、模型调用及重启。CLI 二进制未修改，测试不访问真实 Anthropic 服务。
- `tests/real_oauth_cli.rs`：真实 CLI 从恢复的原生凭据发起模型请求。

本次分析修正了早期 Rust 直接实现 OAuth HTTP 的方案。服务端对 Anthropic 的访问统一归 CLI 执行，Rust 保留 Axum API、RPC 编排和本地存储职责。
