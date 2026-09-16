# 系统提示词与 CLI 原生 prefix/cch

桥接器保持标准 Messages 的 `system` 字符串 / text blocks 输入，由 Claude Code RPC 注入系统提示词正文。每个请求启动独立 CLI，首次初始化发送：

```json
{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","systemPrompt":["本次调用方提供的正文"],"systemPromptSnapshot":false}}
```

这里省略了 MCP、hooks 等其他初始化字段。`systemPromptSnapshot:false` 避免历史会话的提示词快照遮盖新正文；重复 initialize 的成功 ACK 不能当作提示词更新成功，因此不使用这种方式。

## 自动检测边界

| 输入 | 处理 |
|---|---|
| 普通自定义系统提示词 | 保持文本和 text block 顺序，发送 `systemPrompt` |
| 开头的完整 billing 段 + 已知 CLI 身份段 | 删除旧包装，只把正文交给 CLI，避免双份 prefix 或旧 cch |
| 开头仅有已知 CLI 身份段 | 移除重复身份段，由 CLI 重新添加 |
| 多层重复包装 | 逐层去除开头已识别的包装 |
| 正文中引用 billing 字样、未知身份文本或不完整包装 | 原样保留，不凭关键词删除内容 |
| 省略 / null / 空数组 / 包装后无正文 | 发送 `[""]`，明确覆盖内置编码正文 |

检测同时支持独立 text blocks 和使用空行分隔的完整字符串。去除包装时仅删除开头的完整段落，其余正文不 trim、不改写。普通 Messages 不接受 `messages[].role=system/developer`，这些角色仍应由调用方映射到顶层 `system`。

此处的“不同提示词”通过每次显式传递本次正文处理，既不需要保存用户提示词，也不依赖固定的一份官方正文。CLI 的完整内置正文会随版本、模型和配置变化，现有 RPC 没有提供读取完整正文的接口；检测器没有宣称可以识别任意版本的整份默认提示词。

## 原生注入链

```text
Messages system
  → 检测并去除已携带的 CLI 包装
  → initialize.systemPrompt + systemPromptSnapshot=false
  → CLI 组装身份/prefix/billing 文本
  → CLI runtime 填充 cch
  → CLI 请求 Anthropic
```

服务对子进程设置 `CLAUDE_CODE_ATTRIBUTION_HEADER=1`，启用 CLI 原生归因。Rust 不构造 Anthropic HTTP 请求，不实现 cch 算法，不固定 `cc_version` 后缀或 cch，也不接受客户端提供的值作为最终归因结果。旧包装中的值会在检测到完整 CLI 包装时被移除。

`cch` 在 JSON `system` 的 billing 文本内，不是 RPC 字段，也不是 HTTP header。CLI 仍掌握身份文本、字段、缓存分块和归因规则；自定义上游、bare 模式和 CLI 版本可能改变这些行为。普通产品遥测的开关和 attribution 是不同机制。

## 验证

- `tests/system_prompt.rs`：字符串/数组、重复包装、空正文、未知和引用文本的保留。
- `tests/http.rs`：JSON/SSE 两种路径都传入本次正文，关闭快照并开启原生 attribution。
- `tests/real_auth_rpc_smoke.py`：真实 Claude Code 使用本地 TLS fixture，检查新提示词生效、旧正文消失、只有一份 prefix、旧 billing 被重建，以及 cch 被 runtime 填充。
- `tests/container_smoke.py`：在禁用外部网络的容器中运行上述真实 CLI 测试；Actions 对 amd64 和 arm64 都执行。

测试使用合成凭据和本地模拟响应，不验证真实 Anthropic 账号权限或供应商对请求的接受情况。
