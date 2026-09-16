# SDK ↔ CLI 控制请求速查

基于 Claude Desktop `1.52386.6` 的 ASAR，内嵌 Agent SDK `0.3.270`，内置 CLI pin `2.1.270`。整理日期：2026-09-16。

本文汇总 **SDK → CLI 的 42 种控制请求**、**CLI → SDK 的 8 种控制请求**及其响应规则。`mcp_message` 双向出现，因此合计为 49 个不同的 subtype。证据来自包内 SDK 实现；已完成 16 项离线验证，未与真实 CLI 2.1.270 逐项联调。

## 1. 通用消息格式

本地 ProcessTransport 使用 stdin/stdout 上的 NDJSON：每个 JSON 对象一行，发送时追加 `\n`。stderr 用于诊断日志。外层是自定义双向 RPC，MCP 的 JSON-RPC 嵌套在其内部。

双方都可以发送控制请求；以下 JSON 为结构示例，不是抓包。

**请求：**

```json
{"type":"control_request","request_id":"req-1","request":{"subtype":"set_model","model":"example-model"}}
```

**成功响应：**

```json
{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{}}}
```

**处理错误：**

```json
{"type":"control_response","response":{"subtype":"error","request_id":"req-1","error":"error message"}}
```

**取消请求：**

```json
{"type":"control_cancel_request","request_id":"req-1"}
```

| 字段 | 含义 |
|---|---|
| 请求的 `request_id` | 关联本次控制调用，不是 session ID 或工具调用 ID |
| 请求的 `request.subtype` | 控制操作名称 |
| 响应的 `response.request_id` | 原控制请求 ID |
| 响应的 `response.subtype` | `success` 或 `error` |
| 响应的 `response.response` | 成功时的业务返回数据，下文简称 **payload** |
| 响应的 `response.error` | 控制处理错误说明 |

请求支持并发，响应通过 ID 匹配，允许乱序。SDK 普通请求 ID 是随机 base-36 字符串，MCP 主动转发使用 UUID；接收端应把 ID 当成不透明字符串。


## 2. SDK → CLI：42 种请求

表内字段均位于 `request`，省略公共字段 `subtype`。字段列表来自 SDK 发送代码，不构成 CLI 对必填项或类型的完整校验 schema。可选值为 `undefined` 时会在 JSON 序列化中省略。

“返回 payload”表示 SDK 直接交回业务数据，未从该方法确认完整内部结构；“仅等待应答”表示包装方法不返回业务值，**不代表 CLI 不发送响应或响应一定为空**。请求清单是分析摘要，本仓库不包含从第三方安装包提取的源码。

### 2.1 初始化、运行与权限

| subtype | 用途 / SDK 方法 | 请求字段 | SDK 对返回值的处理 |
|---|---|---|---|
| `initialize` | 建立能力与回调配置；`initialize()` / `reinitialize()` | 见第 4 节 | 返回初始化 payload |
| `interrupt` | 中断回合；`interrupt()` | `cancel_queued` | 提取 `still_queued`、可选 `cancelled` 字符串数组；没有前者数组则返回 undefined |
| `stop_task` | 停止指定任务；`stopTask()` | `task_id` | 仅等待应答 |
| `background_tasks` | 请求任务转后台；`backgroundTasks()` | `tool_use_id` | 返回 `payload.backgrounded ?? true` |
| `cancel_async_message` | 取消指定异步消息；`cancelAsyncMessage()` | `message_uuid` | 返回 `payload.cancelled` |
| `set_permission_mode` | 切换权限模式；`setPermissionMode()` | `mode` | 仅等待应答 |
| `set_mcp_permission_mode_override` | 设置 MCP 服务权限覆盖；`setMcpPermissionModeOverride()` | `serverName`, `mode` | 返回 `payload ?? {}` |
| `remote_control` | 开关或重接远程控制；`enableRemoteControl()` | `enabled`, `name`, `reattach_session_id`, `keep_session_on_exit`, `work_secret` | 返回 payload；还读取 `bridge_session_id` 维护关联状态 |

### 2.2 模型、设置与工作区

| subtype | 用途 / SDK 方法 | 请求字段 | SDK 对返回值的处理 |
|---|---|---|---|
| `set_model` | 切换模型；`setModel()` | `model` | 仅等待应答 |
| `set_max_thinking_tokens` | 设置 thinking；`setMaxThinkingTokens()` | `max_thinking_tokens`, `thinking_display` | 仅等待应答 |
| `apply_flag_settings` | 应用设置覆盖；`applyFlagSettings()` | `settings` | 仅等待应答 |
| `get_settings` | 读取设置；`getSettings()` | 无 | 返回 payload |
| `get_hooks_listing` | 读取 Hook 列表；`getHooksListing()` | 无 | 返回 payload |
| `list_permission_rules` | 列出权限规则；`listPermissionRules()` | 无 | 返回 payload |
| `update_settings` | 更新指定来源设置；`updateSettings()` | `source`, `settings` | 仅等待应答 |
| `rewind_files` | 回滚文件；`rewindFiles()` | `user_message_id`, `dry_run` | 返回 payload |
| `seed_read_state` | 注入文件读取状态；`seedReadState()` | `path`, `mtime` | 仅等待应答 |
| `set_cwd` | 切换工作目录；`setCwd()` | `path`, `trust_accepted`, `trusted_directory` | 返回 payload |
| `read_file` | 读取文件；`readFile()` | `path`, `max_bytes`, `encoding` | 返回 payload；请求抛错时包装方法返回 null |

`rewind_files` 是文件回滚操作，不等同于通过 `--resume-session-at` / `--fork-session` 恢复或分叉会话。

### 2.3 MCP、插件与技能

| subtype | 用途 / SDK 方法 | 请求字段 | SDK 对返回值的处理 |
|---|---|---|---|
| `mcp_status` | 获取 MCP 状态；`mcpServerStatus()` | 无 | 返回 `payload.mcpServers` |
| `mcp_set_servers` | 更新服务集合；`setMcpServers()` | `servers` | 返回 payload；发送前处理 SDK 进程内 server 注册 |
| `mcp_reconnect` | 重连服务；`reconnectMcpServer()` | `serverName` | 仅等待应答 |
| `mcp_toggle` | 启停服务；`toggleMcpServer()` | `serverName`, `enabled` | 仅等待应答 |
| `channel_enable` | 启用 channel；`enableChannel()` | `serverName` | 仅等待应答 |
| `mcp_authenticate` | 发起 MCP 认证；`mcpAuthenticate()` | `serverName`, `redirectUri` | 返回 payload |
| `mcp_clear_auth` | 清除 MCP 认证；`mcpClearAuth()` | `serverName` | 返回 payload |
| `mcp_oauth_callback_url` | 提交 OAuth 回调 URL；`mcpSubmitOAuthCallbackUrl()` | `serverName`, `callbackUrl` | 返回 payload |
| `mcp_message` | 转发进程内 MCP 消息；`sendMcpServerMessageToCli()` | `server_name`, `message` | 主动发送分支不等待外层控制应答；详见第 6 节 |
| `reload_plugins` | 重载插件；`reloadPlugins()` | `hold_on_cache_impact` | 返回 payload |
| `reload_skills` | 重载技能；`reloadSkills()` | 无 | 返回 payload |
| `reload_output_styles` | 重载输出风格；`reloadOutputStyles()` | 无 | 返回 payload |

`mcp_set_servers` 对 SDK 进程内服务发送 `{type:"sdk",name,...}` 描述，可带 `timeout`，不把 JavaScript server instance 序列化给 CLI。

### 2.4 认证、查询与辅助功能

| subtype | 用途 / SDK 方法 | 请求字段 | SDK 对返回值的处理 |
|---|---|---|---|
| `claude_authenticate` | 发起 Claude 认证；`claudeAuthenticate()` | `loginWithClaudeAi` | 返回 payload |
| `claude_oauth_callback` | 提交授权码；`claudeOAuthCallback()` | `authorizationCode`, `state` | 返回 payload |
| `claude_oauth_wait_for_completion` | 等待授权完成；`claudeOAuthWaitForCompletion()` | 无 | 返回 payload |
| `get_context_usage` | 查询上下文占用；`getContextUsage()` | SDK 展开调用参数对象；该方法未穷举字段 | 返回 payload |
| `get_usage` | 查询用量；`usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET()` | `skip_behaviors` | 返回 payload |
| `get_memory_dialog` | 获取 memory 对话框数据；`getMemoryDialog()` | 无 | 返回 payload |
| `submit_feedback` | 提交反馈；`submitFeedback()` | `description`, `surface`, `draft_id`, `type`, `title`, `area`, `attach_transcript` | 返回 payload |
| `generate_session_title` | 生成标题；`generateSessionTitle()` | `description`, `persist` | 返回 `payload.title` |
| `side_question` | 发起旁路提问；`askSideQuestion()` | `question`, `history` | 返回 null 或整理后的回答对象；支持 AbortSignal |
| `ultrareview_launch` | 启动 ultrareview；`launchUltrareview()` | `args`, `confirm`，后者默认 false | 返回 payload |
| `message_rated` | 提交消息评价；`messageRated()` | `messageUuid`, `sentiment`, `surface`, `cleared`，后者默认 false | 仅等待应答 |

字段大小写按源码保留。例如 `mcp_toggle.serverName` 和 `mcp_message.server_name` 不同，`message_rated.messageUuid` 也不能改为 `message_uuid`。

`supportedCommands()`、`supportedModels()`、`supportedAgents()`、`accountInfo()`主要读取 initialize 结果；`supportedCommands()`还可使用后续 `system/commands_changed` 缓存。它们没有对应的同名独立控制请求。

## 3. CLI → SDK：8 种请求

SDK 收到这些请求后调用宿主回调，再把返回值放进成功响应的 payload。未支持的 subtype 或回调抛错会生成 `control_response/error`。

| subtype | request 关键字段 | 宿主回调 / 处理 | 成功 payload 与缺省行为 |
|---|---|---|---|
| `can_use_tool` | `tool_name`, `input`, `tool_use_id` 及权限上下文 | `canUseTool(toolName,input,options)` | 允许/拒绝对象，SDK 补 `toolUseID`；回调返回 null 则不回包；缺失回调时报错 |
| `hook_callback` | `callback_id`, `input`, `tool_use_id` | 查找注册的 Hook callback | 直接返回 Hook 结果；找不到 callback ID 则报错 |
| `mcp_message` | `server_name`, `message` | 调 SDK 进程内 MCP transport | `{mcp_response:<JSON-RPC response>}`；服务不存在时报错 |
| `elicitation` | `mcp_server_name`, `message`, `mode`, `url`, `elicitation_id`, `requested_schema`, `title`, `display_name`, `description` | `onElicitation(request,options)` | 回调结果；没有 handler 时 `{action:"decline"}`；null 则不回包 |
| `request_user_dialog` | `dialog_kind`, `payload`, `tool_use_id` | `onUserDialog(request,options)` | 回调结果；没有 handler 或返回 null 时不回包 |
| `oauth_token_refresh` | 无本分支额外读取的请求字段 | `getOAuthToken({signal,onDecline})` | `{accessToken:string/null,reason?:string}`；缺失回调时报错 |
| `host_auth_token_refresh` | 无本分支额外读取的请求字段 | `getHostAuthToken({signal})` | 字符串/null 包装为 `{authToken:...}`，对象则直接返回；缺失回调时报错 |
| `remote_control_work_secret` | `session_id` | 已登记的 work-secret 刷新回调 | `{work_secret:string/null}`；未登记或会话不匹配时报错 |

`can_use_tool` 的完整额外上下文包含 `permission_suggestions`, `blocked_path`, `decision_reason`, `title`, `display_name`, `description`, `default_to_no`, `suppress_always_allow_rule`, `agent_id`, `matched_ask_rule`。SDK 会转换为回调 options，其中部分字段改为 camelCase，并传入 `signal` 和原控制 `requestId`。

OAuth 拒绝 reason 只接受 `signed_out`、`identity_changed`、`transient`、`refresh_failed`。只有 token 为 null 且 handler 给出有效 reason 时，响应才包含该字段。

Code 页的用户对话框 handler 通常返回 `{behavior:"completed",result:<choice>}` 或 `{behavior:"cancelled"}`。注意：SDK 没有 handler 时保持沉默；桌面有 handler 但不认识 dialog kind 或 payload 无法解析时，可以明确返回 cancelled。


## 4. initialize：配置、应答与重投递

| 配置用途 | request 字段 |
|---|---|
| 注册 Hook | `hooks` |
| 注册进程内 MCP | `sdkMcpServers`, `sdkMcpServerConfigs` |
| 输出约束 | `jsonSchema` |
| 系统提示 | `systemPrompt`, `appendSystemPrompt`, `planModeInstructions`, `systemPromptSnapshot`, `appendSubagentSystemPrompt`, `excludeDynamicSections` |
| 工具、代理、技能 | `toolAliases`, `agents`, `skills`, `webSearchIsolationExemptMcpServers` |
| UI 能力 | `title`, `promptSuggestions`, `agentProgressSummaries`, `forwardSubagentText`, `supportedDialogKinds`, `perTaskStopAffordance` |
| 工作区、插件 | `workspaceTrust`, `plugins` |

字符串 `systemPrompt` 在线上转换成数组；`skills` 仅在 SDK 输入为数组时发送。Hook 函数留在宿主进程，线上发送 `hook_0` 等 callback ID，并可附 `matcher` 和 `timeout`。

```json
{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","systemPrompt":["example"],"hooks":{"PreToolUse":[{"matcher":"Read","hookCallbackIds":["hook_0"],"timeout":30}]},"supportedDialogKinds":["refusal_fallback_prompt"]}}
```

初始化 payload 中，SDK 消费 `commands`、`models`、`agents`、`account` 和插件加载确认 `plugins_applied` 等字段，未在这里穷举全部 CLI 返回字段。

初始化应答可另外携带完整待处理请求帧：

```json
{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"commands":[],"models":[],"agents":[],"account":{}},"pending_permission_requests":[{"type":"control_request","request_id":"permission-old","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"file_path":"/example"},"tool_use_id":"toolu_1"}}],"pending_user_dialog_requests":[]}}
```

两个 pending 字段与响应的 `request_id` 同级，**不是 payload 的成员**。SDK 仅从 initialize 应答处理它们：前者仅重投递 `can_use_tool`，后者仅重投递 `request_user_dialog`。普通 RPC 应答及 `awaitControlResponse()` 上的同名字段会被忽略。

`system/init` 是普通消息流中的会话初始化事件，不是 initialize RPC 应答。普通 `query()` 会先发 initialize，然后启动用户输入流，但不统一等待握手应答；warm-query 工厂则明确等待初始化，默认期限 60 秒。


## 5. can_use_tool：拒绝也是正常业务响应

**CLI 请求：**

```json
{"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"file_path":"/example.txt"},"tool_use_id":"toolu_1","permission_suggestions":[]}}
```

**SDK 允许：**

```json
{"type":"control_response","response":{"subtype":"success","request_id":"permission-1","response":{"behavior":"allow","updatedInput":{"file_path":"/example.txt"},"toolUseID":"toolu_1"}}}
```

**SDK 拒绝：**

```json
{"type":"control_response","response":{"subtype":"success","request_id":"permission-1","response":{"behavior":"deny","message":"User rejected Read","interrupt":true,"toolUseID":"toolu_1"}}}
```

外层 `success` 表示控制请求已正常处理，是否允许工具执行由 payload 的 `behavior` 决定。允许结果可以携带 `updatedPermissions`，桌面还可能加入 `decisionClassification`。

UI 的 `once` / `always` / `scheduled` 不会作为同名 verdict 直接发送：它们转换为 `behavior:"allow"`，并按决定附加权限更新。Code 页 UI 权限 `requestId` 是主进程另外生成的 UUID；主进程通过挂起的回调关联原 CLI `request_id`，两者不能混用。`tool_use_id` 则是第三个独立标识。

## 6. mcp_message：两层请求 ID

```json
{"type":"control_request","request_id":"outer-1","request":{"subtype":"mcp_message","server_name":"demo","message":{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}}}
{"type":"control_response","response":{"subtype":"success","request_id":"outer-1","response":{"mcp_response":{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}}}}
```

外层用 `request_id` 关联控制调用，内层用 JSON-RPC `id` 关联 MCP 操作。SDK 内部以 `server_name + ":" + message.id` 匹配待处理的 MCP 响应。

宿主 MCP server 发出消息时，如果命中内层 pending ID，就解决原请求，由原外层控制调用回包；否则生成新的 `control_request/mcp_message` 主动发送给 CLI。

CLI 发来的内层消息不满足 request 判定（含 `method`、含 `id` 且 id 不为 null）时，SDK 交给 MCP transport，并返回占位 `mcp_response:{jsonrpc:"2.0",result:{},id:0}`。该占位不是所有 MCP 消息的固定 ID。


## 7. 取消、重复请求与超时

| 情况 | 包内 SDK 行为 |
|---|---|
| 收到 `control_cancel_request` | abort 对应 callback signal，并移除 cancel controller；没有固定取消 ACK |
| callback 被取消后继续返回结果 | 仍可能发送最终响应；仅 signal 已 aborted 不会自动禁止回包 |
| 可沉默的 callback 返回 null | 不发送响应；适用于权限、elicitation、用户对话框分支 |
| 同 ID 请求在处理期间重复到达 | 跳过重复调用 |
| 同 ID 请求在处理完成后再次到达 | 可以重新执行，没有永久请求去重缓存 |
| 出站请求的 AbortSignal 取消 | 移除 pending 等待、拒绝 Promise，并尽力发送取消帧 |
| 收到未匹配的控制响应 | 缓存最多 1024 项，供 `awaitControlResponse(id)` 获取；超限淘汰最早插入项 |
| 通用控制请求超时 | `Query.request()` 自身没有统一超时；具体工厂或调用方可另加期限 |
| Query 关闭 | abort 回调，拒绝待处理请求，并关闭 transport |

`interrupt` 是一个要等待控制应答的回合中断 RPC；`control_cancel_request` 取消指定控制调用；`cancel_async_message` 取消指定输入消息。三者作用对象不同。控制请求的成功应答也不表示模型回合结束，后者仍需要处理消息流中的 `result`。


## 8. 容易混入控制请求表的其他帧

| 帧 | 与控制 RPC 的区别 |
|---|---|
| `user` | 发送普通输入，使用消息 UUID / session ID，不以控制 request_id 等待应答 |
| `assistant`, `stream_event`, `result`, `system` 等 | 普通输出事件；不属于控制 subtype |
| `keep_alive` | SDK reader 忽略，不传给普通消息迭代器 |
| `transcript_mirror` | 交给 transcript mirror batcher，不传给普通消息迭代器 |
| `update_environment_variables` | 桌面主动更新 CLI 环境的顶层帧，不包装在 control_request 内 |

运行中 token 更新示例：

```json
{"type":"update_environment_variables","variables":{"CLAUDE_CODE_OAUTH_TOKEN":"<redacted>"}}
```

这个主动推送与 CLI 发起的 `oauth_token_refresh` 是两条独立路径。前者没有控制 request_id，经过桌面输入队列；后者使用正常控制请求/响应。

## 9. 证据与验证范围

以上内容归纳自指定版本安装包中的 SDK 实现及单独的本地分析，分析时的离线函数验证为 16/16 通过。本仓库提供协议摘要与独立实现，不分发原始 ASAR、CLI 二进制、提取后的第三方源码或依赖这些文件的离线验证脚本。

此清单表示该版本 SDK 暴露和处理的协议能力，不代表每项都被桌面实际调用，也不代表其他 CLI 版本支持相同字段。对于表中直接透传的 payload，完整返回 schema 仍需结合对应 CLI 实现或真实联调确认。本仓库可运行的适配器验证见 [README](../README.md#验证)。

## 10. Rust / Axum Messages 适配实践（CLI 2.1.272）

已实现 [claude-messages-bridge](../README.md)，采用 stdio 控制 RPC，向外提供 `POST /v1/messages` JSON / SSE，并由调用方执行工具、传回 `tool_result`。这是后续本地 CLI 2.1.272 的联调结果，不改变上文 ASAR 内 SDK 的版本范围。

- initialize 完成之前，CLI 就可能发出 `mcp_message` 请求；读循环必须在等待初始化时处理 MCP 回调，避免双方互等。
- `sdkMcpServers:["messages"]` 配合 MCP initialize、tools/list 注册工具。CLI 侧使用 `mcp__messages__<name>`，HTTP 响应映射回调用方名称。
- 原生 transcript 恢复会修补未完成工具轮次。适配器先写入包含最终 `tool_result` 的完整历史，再用 `--resume-session-at <上一条 assistant UUID>` 回到该 assistant，初始化后通过 stdin 发送最终 user 消息。直接恢复缺少结果的 tool_use，会导致 CLI 修补或丢弃工具记录。
- Messages API 每次只返回一条模型消息，因此以首个完整 `stream_event.event.message_stop` 为完成点，随后中断并回收进程；完整 agent 运行仍以 CLI `result` 为生命周期结束标志，两种场景不同。
- `--tools ""` 关闭内置工具；权限回调返回业务 deny；MCP tools/call 没有执行实现；`--max-turns 1` 限制后续 agent 循环。
- 2.1.272 仍向上游内容添加日期提醒和缓存标记，日期可能追加到最后一个 tool_result。该实现兼容 Messages 核心结构，但不承诺上游请求与直连 API 逐字相同。

验证脚本：[真实 CLI 联调](../tests/real_cli_smoke.py)。2026-09-16 已通过 7 次真实 CLI → 本机模拟 Anthropic API 请求，覆盖文本历史、max_tokens、JSON/SSE、并行工具、工具结果、错误结果、混合文本及 tool_choice none；使用隔离配置与 dummy key，没有真实模型调用。另有 Rust 测试覆盖协议、HTTP 错误、子进程回收、OAuth 和 redb；验证方式见 README。
