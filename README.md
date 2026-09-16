# Claude Messages Bridge

> [!WARNING]
> **使用本项目可能违反 Anthropic 的用户协议或相关服务条款，并可能导致账号被限制、暂停或封禁（封号）。**
> 本项目是非官方实现，未获 Anthropic 授权或认可，不保证账号安全或服务持续可用。使用前请阅读并确认适用于你的账号及使用方式的 [消费者服务条款](https://www.anthropic.com/legal/consumer-terms) 和 [商业服务条款](https://www.anthropic.com/legal/commercial-terms)，自行评估并承担使用风险。

Rust + Axum 实现的 Claude Code CLI RPC 客户端，同时提供 Anthropic Messages 风格的 HTTP 服务、React 管理控制台和 OAuth 账号登录。本地 OAuth 凭据持久化到 redb。

```text
HTTP 调用方 → POST /v1/messages → Axum → stdio NDJSON RPC → Claude Code CLI
                                      ← stream_event ← 模型
```

工具采用标准 Messages 往返：响应返回 `tool_use`，调用方执行工具，然后携带完整历史及 `tool_result` 再次请求。桥接器不执行工具。支持 JSON 响应和 Anthropic SSE。

这是一套兼容 Messages 核心结构的 CLI 适配器，并非全部 Anthropic API 参数的无损代理。CLI RPC 和恢复格式基于本地 **Claude Code 2.1.272** 验证；升级 CLI 后请重新运行真实 CLI 测试。

## 启动

需要 Rust 1.88+、Node.js 22.12+、Python 3 和已安装的 Claude Code。

```sh
git clone https://github.com/StarryKira/claude-messages-bridge.git
cd claude-messages-bridge
python3 scripts/init-env.py
bash scripts/build.sh
bash scripts/run-console.sh
```

初始化脚本生成 `.env`（权限 `0600`）中的随机 `BRIDGE_ADMIN_TOKEN` 和独立的 `BRIDGE_API_KEY`，不会覆盖已有配置或打印密钥。启动脚本加载 `.env`，数据库位于 `.bridge/credentials.redb`；数据库文件权限为 `0600`，运行目录为 `0700`。

打开 [Web 控制台](http://127.0.0.1:8787/admin/)，使用 `.env` 中的 `BRIDGE_ADMIN_TOKEN` 解锁，点击「开始 OAuth 登录」。在 Claude 官方页面完成授权，将页面提供的完整 `code#state` 粘贴回控制台。可以选择 Claude 订阅或 Anthropic Console；邮箱和企业 SSO 在官方页面选择。凭据保存在服务器的 redb 中，重启服务后仍然可用。每个实例只负责一个账号：首次成功登录后固定绑定 CLI 返回的账号 UUID，以后仅接受同账号重新授权。退出和重启都保留绑定；登录其他账号会失败，并保留原凭据。

默认监听 `127.0.0.1:8787`。`GET /healthz` 仅检查服务存活，不检查 CLI 登录或模型权限。示例请求需要先加载网关密钥：

```sh
set -a; source .env; set +a
curl http://127.0.0.1:8787/v1/messages \
  -H "x-api-key: $BRIDGE_API_KEY" \
  -H 'content-type: application/json' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"claude-sonnet-4-6","max_tokens":256,"messages":[{"role":"user","content":"用一句话介绍 Rust"}]}'
```

流式请求添加 `"stream":true`，并用 `curl -N` 关闭客户端缓冲。服务发送 `message_start`、`content_block_start/delta/stop`、`message_delta`、`message_stop` 事件，不发送 `[DONE]`。模型 usage 取自模型事件；不会将 CLI 多轮累计 usage 当作单条消息 usage。

服务支持常规 Anthropic 客户端的自定义 `base_url`，例如 Python SDK：

```python
import os
from anthropic import Anthropic

client = Anthropic(base_url="http://127.0.0.1:8787", api_key=os.environ["BRIDGE_API_KEY"])
message = client.messages.create(
    model="claude-sonnet-4-6",
    max_tokens=256,
    messages=[{"role": "user", "content": "你好"}],
)
print(message.content)
```

`BRIDGE_API_KEY` 仅用于桥接器鉴权，不会替换账号的上游凭据。`BRIDGE_ADMIN_TOKEN` 只用于账号管理接口，不能代替 Messages 密钥。

仅使用原来的 CLI 认证环境时，可不设置 `BRIDGE_ADMIN_TOKEN` 和 `BRIDGE_CREDENTIAL_DB`，直接运行二进制。此时关闭管理接口，继续继承 CLI 已有登录或服务端 API key。直接运行二进制不会自动加载 `.env`。

## 一个实例，一个账号

实例以自己的 redb 文件保存账号绑定，不提供账号列表、切换或轮询。`BRIDGE_MAX_CONCURRENCY` 控制同一账号的并发请求数，不代表账号数。多个实例可以复用同一个 CLI 二进制和前端构建，各自拥有独立的认证缓存和凭据。

| 配置 | 实例 A | 实例 B |
|---|---|---|
| `BRIDGE_BIND` | `127.0.0.1:8787` | `127.0.0.1:8788` |
| `BRIDGE_CREDENTIAL_DB` | `.bridge/account-a.redb` | `.bridge/account-b.redb` |
| `BRIDGE_ADMIN_TOKEN` | A 的独立管理令牌 | B 的独立管理令牌 |
| `BRIDGE_API_KEY` | A 的独立调用密钥 | B 的独立调用密钥 |

分别把配置写入私有的 `.env.account-a` 和 `.env.account-b`，在两个终端加载对应文件并直接启动同一个二进制：

```sh
set -a
source .env.account-a  # 另一个终端使用 .env.account-b
set +a
./target/release/claude-messages-bridge
```

令牌分别生成，管理令牌至少 32 字节且不能与调用密钥相同。配置文件权限设为 `0600`。分别打开两个端口的 `/admin/`，登录各自的账号。redb 有独占锁，两个进程不能共用同一数据库。退出只删除登录凭据，不解除绑定；另一个账号请使用新的实例配置和数据库，不覆盖原库。

以上固定绑定由 redb 模式执行；前述继承本机 CLI 的兼容模式没有 redb 绑定校验，须使用专属 CLI 认证环境。Web 控制台默认使用 redb 模式。

## OAuth 与 redb

服务端所有发往 Anthropic 的请求都由 Claude Code CLI 执行。Rust 只通过 stdin/stdout 的控制 RPC 驱动 CLI，不实现 OAuth HTTP、不硬编码 client ID、不生成 PKCE verifier，也不直接请求 token、profile 或 Console API key 端点。详细信封见 [OAuth RPC / redb 协议说明](docs/oauth-redb.md)。

登录使用 `claude_authenticate` 获取 CLI 生成的官方授权 URL；提交授权码时调用 `claude_oauth_callback`，由 CLI 完成 PKCE 校验、token 交换、账号查询、组织策略校验和原生凭据保存。`claude_oauth_callback` 的应答已经等待整个登录流程完成；自动回调场景可用 `claude_oauth_wait_for_completion`，本控制台采用手动授权码方式。当前 CLI 的账号认证 RPC 只接收 `loginWithClaudeAi`，所以本地表单不再传入邮箱或 SSO 参数。

redb 的 `oauth_credentials_v1` 表以 `active` 保存账号凭据、CLI 原生 token 字段和必要的账号配置，以 `binding` 保存实例绑定身份。已有版本的记录可直接读取，并在运行时转为 CLI 原生格式。授权码、state 和尚未完成的授权会话仅在内存中，重启后需要重新发起授权。

CLI 需要原生凭据存储才能自主刷新 token。服务为当前账号创建权限 `0700` 的临时工作目录，凭据文件权限为 `0600`；从 redb 恢复该目录，并在 CLI 初始化、模型响应完成及请求清理时把 CLI 写出的轮换凭据保存回 redb。并发 CLI 共享这个认证缓存，使用 CLI 自带的跨进程刷新锁和 token 比较更新，避免复制旧 refresh token 后重复刷新。每个模型请求仍有独立的工作目录与会话。

macOS 的 CLI 原生调用 `security` 存取凭据；本项目通过仅对子进程生效的私有 PATH 适配器，将这两类凭据服务映射到临时文件，不访问用户的系统 keychain。该适配器只做本地存储，不实现 OAuth 或网络请求；需要 Python 3。Linux 使用 CLI 原生凭据文件。持久化来源为 redb；正常退出和账号退出会清理临时缓存，强制杀死整个服务或系统崩溃可能遗留临时文件，且无法保证尚未同步的刷新结果已写入 redb。redb 本身没有额外静态加密。

开始登录时需要没有正在运行的模型请求；授权期间暂停接收新的模型请求。取消、超时、登录失败或账号 UUID 不匹配时保留原凭据；同账号重新授权成功后在一个事务中更新凭据。退出清除 redb 的 `active` 凭据记录和临时缓存，保留 `binding` 账号身份，不向 Anthropic 发送撤销请求。账号状态展示最近一次 CLI 返回并保存的元数据，不表示实时在线验证。管理令牌和网关密钥仍通过服务环境变量配置；OAuth token 不返回浏览器。

管理接口统一使用 `Authorization: Bearer <BRIDGE_ADMIN_TOKEN>`，响应设置 `Cache-Control: no-store`，只返回白名单账号信息。前端令牌只在当前页面内存保存，不进入 localStorage、sessionStorage、cookie 或 URL，刷新页面需要重新解锁。

| 接口 | 用途 |
|---|---|
| `GET /api/admin/status` | 账号、实例绑定、服务状态及当前登录会话；`account.binding` 在未绑定时为 null，绑定后含 `account_id` 和 `email` |
| `POST /api/admin/oauth/start` | `{ "method": "claudeai" }` 或 `{ "method": "console" }`；返回登录 ID 和官方授权链接 |
| `GET /api/admin/oauth` | 查询当前授权进度 |
| `POST /api/admin/oauth/{id}/code` | `{ "code": "code#state" }`；校验 state 后调用 CLI 的 `claude_oauth_callback` |
| `DELETE /api/admin/oauth/{id}` | 取消当前授权 |
| `POST /api/admin/logout` | 清除当前凭据并保留实例账号绑定 |

前端开发：先运行后端，再执行 `npm run dev --prefix web`，打开 `http://127.0.0.1:5173/admin/`；Vite 将 `/api` 请求代理到 `127.0.0.1:8787`。生产静态文件由 Axum 同源提供。

## 工具往返

完整可运行示例见 [examples/tool_roundtrip.py](examples/tool_roundtrip.py)，仅使用 Python 标准库：

```sh
python3 examples/tool_roundtrip.py
# 若服务启用了鉴权：
BRIDGE_API_KEY=your-bridge-key python3 examples/tool_roundtrip.py
```

1. 请求中通过 `tools` 传入 `name`、`description`、`input_schema`。
2. 响应 `stop_reason: "tool_use"`，保留响应的整个 `content` 作为 assistant 消息。
3. 调用方执行每个工具；对每个 ID 返回一个 `tool_result`，全部放入下一条 user 消息，位于其他文本之前。
4. 再发送完整 `messages` 历史。支持并行 tool_use、文本与工具混合内容，以及 `is_error: true`。

例如下一条消息：

```json
{
  "role": "user",
  "content": [
    {"type": "tool_result", "tool_use_id": "toolu_example", "content": "上海，晴，25°C"}
  ]
}
```

工具通过 SDK MCP 注册为 `mcp__messages__<name>`。返回 HTTP 时恢复调用方原名；tool ID 不改变。`can_use_tool` 一律拒绝，MCP `tools/call` 不提供执行实现。收到第一条完整模型消息便中断并回收 CLI，另以 `--max-turns 1` 限制 agent 循环。

## 兼容范围

| 参数 / 内容 | 行为 |
|---|---|
| `model` | 交给 CLI 选择；别名、可用模型及权限由 CLI 决定 |
| `max_tokens` | 必须大于 0，映射到 `CLAUDE_CODE_MAX_OUTPUT_TOKENS`；真实 CLI 测试验证了 128 原值进入上游 |
| `messages` | 保留结构与角色；合并连续相同角色；首尾必须为 user，不支持 assistant prefill |
| `system` | 字符串或 text blocks，经 initialize 的 `systemPrompt` 替换默认编码提示 |
| `stream` | JSON 或 SSE，默认 false |
| `tools` | 自定义工具名、描述、object JSON Schema；不支持服务端工具或工具 beta 扩展 |
| `tool_choice` | `auto` / `none`；none 通过不注册工具实现 |
| `thinking` | disabled / adaptive / enabled + budget_tokens；enabled 要求 1024 ≤ budget < max_tokens，默认 disabled |
| text | user / assistant 文本 |
| image | user 中 base64 PNG/JPEG/GIF/WebP，包含 tool_result 内的图片；图片有效性最终由 CLI/模型校验 |
| tool_use / tool_result | 工具 ID 配对、并行调用、错误结果、结果中的 text/image blocks |
| thinking / redacted_thinking | assistant 历史块，保留签名 / data；响应重组 thinking/signature delta |

未支持字段返回 **400**，不会静默丢弃。例如 `temperature`、`top_p`、`top_k`、`stop_sequences`、`metadata`、`cache_control`、`output_config`、`service_tier`、`context_management`、`tool_choice:any/tool`、`disable_parallel_tool_use`、document、URL 图片及 `anthropic-beta`。也没有 `/v1/messages/count_tokens` 或 `/v1/models`。

`anthropic-version` 可省略；提供时仅接受 `2023-06-01`。

**CLI 固有差异：** CLI 仍会添加自身的身份信息、日期提醒、缓存标记等。2.1.272 的真实测试观察到，日期可能追加到最后一个 tool_result 文本中，或成为 user 消息中的额外 text block。结构、工具 ID、结果内容和角色得到保留，但上游请求不保证与直连 Messages API 逐字相同。该服务不修改 CLI 二进制。

## RPC 与历史恢复

每个 HTTP 请求独立启动一个 CLI，使用私有临时目录和随机 session ID。带历史的请求写入原生 JSONL transcript，保留父子 UUID、role 和内容块。

工具恢复有一个必须处理的顺序：CLI 先修补 transcript 中未配对的 tool_use，再应用 `--resume-session-at`。因此先把**完整请求历史（包括最终 tool_result）**写入 transcript，保证校验时工具成对；再定位到倒数第二条 assistant 的 UUID，最后在 initialize 成功后从 stdin 发送最终 user 消息一次。直接恢复未配对的 tool_use 再发送 tool_result，会导致 CLI 删除或改写工具轮次。

初始化过程中也必须读取并回答 CLI 的 `mcp_message`，因为 MCP 初始化可能早于 initialize 的最终应答。外层使用 SDK 控制信封，内层才是 MCP JSON-RPC 2.0；二者的请求 ID 相互独立。

模块分工：

| 文件 | 职责 |
|---|---|
| `src/lib.rs` | Axum 路由、鉴权、并发额度、JSON / SSE 输出 |
| `src/rpc.rs` | CLI 生命周期、initialize、中断、MCP / 权限回调、逐行收发 |
| `src/history.rs` | 原生 transcript、完整工具配对及恢复锚点 |
| `src/request.rs` | 请求校验、工具名称与 schema 映射 |
| `src/response.rs` | 内容增量、tool JSON、thinking、usage 重组与流完整性校验 |
| `src/config.rs` | 服务环境变量 |
| `src/admin.rs` | 管理鉴权、OAuth 会话状态、账号操作与模型请求互斥 |
| `src/oauth.rs` | CLI 认证 RPC 编排、redb 与临时缓存同步 |
| `src/control.rs` | 账号管理所用的 stdio 控制 RPC 客户端 |
| `src/credential_cache.rs` | CLI 原生凭据恢复、读取与临时缓存生命周期 |
| `scripts/credential-store.py` | macOS 子进程的私有凭据存储适配器 |
| `src/store.rs` | redb 凭据事务与公开账号信息过滤 |
| `web/src/` | React 控制台、授权表单、API 接入说明 |

控制请求完整清单见 [SDK–CLI 控制请求文档](docs/sdk-cli-control-requests.md)。

## 配置与错误

| 环境变量 | 默认 / 说明 |
|---|---|
| `BRIDGE_BIND` | `127.0.0.1:8787` |
| `CLAUDE_CLI_PATH` | 优先 `$HOME/.local/bin/claude`，不存在则从 PATH 查找 `claude` |
| `BRIDGE_API_KEY` | 未设置；启用后支持 `x-api-key` 或 `Authorization: Bearer ...` |
| `BRIDGE_ADMIN_TOKEN` | 未设置则关闭管理 API；启用时至少 32 字节，必须与 Messages key 不同 |
| `BRIDGE_CREDENTIAL_DB` | 管理启用时默认 `$HOME/.claude-messages-bridge/credentials.redb`；启动脚本配置为 `.bridge/credentials.redb` |
| `BRIDGE_WEB_DIR` | 默认构建源码目录下的 `web/dist`；移动二进制后应显式配置静态文件路径 |
| `BRIDGE_OAUTH_TIMEOUT_SECONDS` | 600；授权会话总有效期 |
| `BRIDGE_MAX_CONCURRENCY` | 4；满额直接返回 429 |
| `BRIDGE_TIMEOUT_SECONDS` | 180；包含启动、初始化和生成 |
| `BRIDGE_INIT_TIMEOUT_SECONDS` | 30 |
| `BRIDGE_CLI_BARE` | 0；设为 1 追加 `--bare`，跳过常规 OAuth/keychain 登录，需要显式 API 凭据；管理 OAuth 模式必须为 0 |
| `RUST_LOG` | info |

绑定非 loopback 地址必须设置 `BRIDGE_API_KEY`。服务使用 HTTP；需要外部访问时由反向代理提供 TLS。

请求体上限 32 MiB，单个 CLI 帧上限 16 MiB，输出事件累计上限 32 MiB。管理请求体上限 8 KiB。CLI 的本地工具、普通配置来源、自动压缩、hooks、CLAUDE.md、自动记忆、自动文件附件、Chrome 和外部 MCP 配置均在启动时关闭。CLI 的组织策略及模型权限仍由上游处理。

客户端断开或请求超时会取消并回收子进程；SIGINT/SIGTERM 取消正在执行的请求。请求临时目录随请求清理，启用 `--no-session-persistence`。管理模式下，CLI 的原生认证文件和诊断文件写入私有临时缓存；继承本机 CLI 登录的非管理模式仍可能写入其全局目录。

| 情况 | HTTP / SSE |
|---|---|
| 非法或未支持参数 | 400 `invalid_request_error` |
| 网关密钥错误 | 401 `authentication_error` |
| 请求体过大 | 413 `request_too_large` |
| 并发已满 | 429 `rate_limit_error` |
| CLI 启动、初始化、上游或协议失败 | 502 `api_error` |
| 总超时 | 504 `api_error` |
| 已开始 SSE 后失败 | `event: error`，不会伪造成功的 message_stop |

桥接器不记录 prompt 或 CLI stderr 内容；CLI 退出诊断会提示检查服务器端登录状态和 CLI 版本。每个 Messages HTTP 响应带独立 `request-id`。

当前启动 CLI 时固定设置 `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`，关闭常规产品遥测；显式配置的企业 OTLP 是独立链路，父进程的 `CLAUDE_CODE_ENABLE_TELEMETRY` 和 `OTEL_*` 环境变量仍会继承。收到完整模型消息后会直接终止 CLI，不保证退出前完成遥测队列发送。关闭遥测不代表绕过供应商风控或消除封号风险。

## 验证

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 tests/real_cli_smoke.py --cli "$HOME/.local/bin/claude"
python3 tests/real_auth_rpc_smoke.py --cli "$HOME/.local/bin/claude"
REAL_CLAUDE_CLI="$HOME/.local/bin/claude" cargo test --locked --test real_oauth_cli -- --ignored
cargo build --locked
npm ci --prefix web
npm run build --prefix web
cd web
npx playwright install chromium
npm test
# 已安装 Google Chrome 时可改用：PLAYWRIGHT_CHANNEL=chrome npm test
```

Rust 测试使用假 CLI，覆盖 RPC 信封、工具往返、JSON/SSE、usage、thinking 签名、错误、超时、超大帧、鉴权、并发和断开连接后的进程回收，以及账号认证 RPC 顺序、state 校验、单次授权码、redb 重启恢复、刷新写回、取消与超时、Console key、退出清除、实例永久绑定、旧库迁移、同账号重新授权、跨账号拒绝和多实例隔离，以及错误脱敏。Playwright 检查管理路由、授权表单、令牌驻留范围、账号展示及桌面/手机布局。

`real_auth_rpc_smoke.py` 使用真实 CLI 和本地 HTTPS 测试服务，临时 CA 仅注入测试子进程。代理终止 TLS 后直接生成模拟响应，绝不转发请求到外网。它验证 CLI 原生 OAuth RPC、PKCE、账号查询、刷新、Console API key、模型请求，以及 redb 重启恢复；需要本机 `openssl` 命令，不使用真实账号。

`real_oauth_cli` 单独验证真实 CLI 使用由 redb 恢复的原生凭据，推理 HTTP 请求只发往本地假 API。以上测试没有执行真实账号授权。

`real_cli_smoke.py` 使用**真实 CLI + 本机假 Anthropic HTTP 服务**，隔离 HOME/配置并使用 dummy API key，不调用云端模型。它检查结构化历史、max_tokens、SSE、MCP 注册、并行工具、tool_result 恢复、错误工具结果、附带文本与 tool_choice none，同时断言没有多余的 agent 模型轮次。它不验证实际账号登录、模型权限或云端模型质量。

API 格式参考：[Anthropic Messages](https://platform.claude.com/docs/en/api/messages/create)、[Anthropic Streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)、[Axum SSE](https://docs.rs/axum/latest/axum/response/sse/index.html)。
