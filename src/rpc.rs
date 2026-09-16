use crate::{
    config::Config,
    error::{ApiError, Result},
    history::seed_history,
    request::{MCP_SERVER, MessagesRequest},
    response::{MessageAccumulator, translate_event},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{OwnedSemaphorePermit, mpsc},
};
use tokio_util::{
    codec::{FramedRead, LinesCodec},
    sync::CancellationToken,
};
use uuid::Uuid;

pub struct Session {
    pub events: mpsc::Receiver<Result<Value>>,
    pub cancel: tokio_util::sync::DropGuard,
}

pub fn start(
    config: Arc<Config>,
    oauth: Arc<crate::oauth::OAuthClient>,
    request: MessagesRequest,
    permit: OwnedSemaphorePermit,
    shutdown: CancellationToken,
) -> Session {
    let cancel = shutdown.child_token();
    let guard = cancel.clone().drop_guard();
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let _permit = permit;
        let cache = match oauth.inference_cache().await {
            Ok(cache) => cache,
            Err(error) => {
                let _ = tx.send(Err(error)).await;
                return;
            }
        };
        let work = run(&config, &oauth, cache.as_ref(), request, &tx, &cancel);
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(()),
            r = tokio::time::timeout(config.request_timeout, work) => r.unwrap_or_else(|_| Err(ApiError::timeout())),
        };
        // Persist native CLI token rotation even on errors or client disconnect.
        let persisted = if let Some(cache) = &cache {
            oauth.sync(cache).await
        } else {
            Ok(())
        };
        if let Err(error) = result.and(persisted) {
            tracing::warn!(error_type = error.kind, "CLI request failed");
            tokio::select! { _ = cancel.cancelled() => {}, _ = tx.send(Err(error)) => {} }
        }
    });
    Session {
        events: rx,
        cancel: guard,
    }
}

async fn run(
    config: &Config,
    oauth: &crate::oauth::OAuthClient,
    cache: Option<&Arc<crate::credential_cache::CredentialCache>>,
    req: MessagesRequest,
    tx: &mpsc::Sender<Result<Value>>,
    cancel: &CancellationToken,
) -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("claude-messages-")
        .tempdir()
        .map_err(|_| ApiError::upstream("Cannot create request workspace"))?;
    let sid = Uuid::new_v4().to_string();
    let history = seed_history(&req, directory.path(), &sid).await?;
    let mut command = Command::new(&config.cli);
    config.apply_cli_env(&mut command);
    if let Some(cache) = cache {
        cache.apply(&mut command)?;
    }
    command.args([
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--setting-sources",
        "",
        "--settings",
        "{\"disableAllHooks\":true}",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--no-chrome",
        "--max-turns",
        "1",
    ]);
    command.arg("--model").arg(&req.model);
    if config.bare {
        command.arg("--bare");
    }
    if let Some((path, resume_at)) = history {
        command
            .arg("--resume")
            .arg(path)
            .arg("--resume-session-at")
            .arg(resume_at);
    } else {
        command.arg("--session-id").arg(&sid);
    }
    let thinking = req.thinking.as_ref();
    match thinking.and_then(|t| t["type"].as_str()) {
        Some("enabled") => {
            command
                .arg("--max-thinking-tokens")
                .arg(thinking.unwrap()["budget_tokens"].to_string());
        }
        Some("adaptive") => {
            command.args(["--thinking", "adaptive"]);
        }
        _ => {
            command.args(["--thinking", "disabled"]);
        }
    }
    command
        .current_dir(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", req.max_tokens.to_string())
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("DISABLE_AUTO_COMPACT", "1")
        .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("CLAUDE_CODE_DISABLE_ATTACHMENTS", "1")
        .env("ENABLE_TOOL_SEARCH", "false")
        .env("CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION", "0")
        .env_remove("CLAUDE_CODE_RESUME_INTERRUPTED_TURN")
        .env_remove("CLAUDE_CODE_RESUME_FROM_SESSION")
        .env_remove("CLAUDE_CODE_SDK_HAS_OAUTH_REFRESH")
        .env_remove("CLAUDE_CODE_SDK_HAS_HOST_AUTH_REFRESH");
    let mut process = Process {
        child: command
            .spawn()
            .map_err(|e| ApiError::upstream(format!("Cannot start Claude Code: {e}")))?,
        stderr: None,
    };
    let mut stdin = process
        .child
        .stdin
        .take()
        .ok_or_else(|| ApiError::upstream("CLI stdin unavailable"))?;
    let stdout = process
        .child
        .stdout
        .take()
        .ok_or_else(|| ApiError::upstream("CLI stdout unavailable"))?;
    // Always drain stderr, but do not expose prompts, environment or credentials in logs.
    if let Some(mut stderr) = process.child.stderr.take() {
        process.stderr = Some(tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
            }
        }));
    }
    let mut lines = FramedRead::new(
        stdout,
        LinesCodec::new_with_max_length(config.max_frame_bytes),
    );
    let init_id = Uuid::new_v4().to_string();
    write(&mut stdin, &json!({"type":"control_request","request_id":init_id,"request":{
        "subtype":"initialize","systemPrompt":req.system_parts()?,"hooks":{},
        "sdkMcpServers":if req.active_tools().is_empty() { Vec::<&str>::new() } else { vec![MCP_SERVER] },
        "supportedDialogKinds":[],"promptSuggestions":false,"excludeDynamicSections":true
    }})).await?;
    let mut initialized = false;
    let init_deadline = tokio::time::Instant::now() + config.init_timeout;
    let tools = req.tool_map();
    let mut accumulator = MessageAccumulator::default();
    let mut output_bytes = 0usize;
    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep_until(init_deadline), if !initialized => return Err(ApiError::upstream("CLI initialize RPC timed out")),
            line = lines.next() => line,
        };
        let Some(line) = line else {
            return Err(ApiError::upstream(
                "Claude Code exited before a complete model response; check server-side CLI authentication and version",
            ));
        };
        let line =
            line.map_err(|_| ApiError::upstream("CLI frame exceeded limit or stdout read failed"))?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match frame["type"].as_str() {
            Some("control_request") => {
                let response = control_reply(&frame, &req)?;
                write(&mut stdin, &response).await?;
            }
            Some("control_response") if frame["response"]["request_id"] == init_id => {
                if initialized {
                    continue;
                }
                if frame["response"]["subtype"] != "success" {
                    return Err(ApiError::upstream("CLI rejected initialize RPC"));
                }
                if let Some(cache) = cache {
                    oauth.sync(cache).await?;
                }
                initialized = true;
                let last = req.messages.last().unwrap();
                write(
                    &mut stdin,
                    &json!({"type":"user","session_id":sid,"uuid":Uuid::new_v4().to_string(),
                    "parent_tool_use_id":null,"message":{"role":"user","content":last.content}}),
                )
                .await?;
            }
            Some("stream_event") if frame["parent_tool_use_id"].is_null() => {
                if !initialized {
                    return Err(ApiError::upstream(
                        "CLI model output before initialize response",
                    ));
                }
                let mut event = frame["event"].clone();
                translate_event(&mut event, &tools)?;
                accumulator.push(&event)?;
                output_bytes = output_bytes.saturating_add(line.len());
                if output_bytes > config.max_output_bytes {
                    return Err(ApiError::upstream(
                        "Model output exceeded bridge memory limit",
                    ));
                }
                let complete = accumulator.complete();
                if complete && let Some(cache) = cache {
                    oauth.sync(cache).await?;
                }
                tx.send(Ok(event))
                    .await
                    .map_err(|_| ApiError::upstream("HTTP client disconnected"))?;
                if complete {
                    // Anthropic Messages requests produce one model message, not an agent loop.
                    // No tools/call implementation exists; terminate before the CLI continues.
                    let _ = write(
                        &mut stdin,
                        &json!({"type":"control_request","request_id":Uuid::new_v4().to_string(),
                        "request":{"subtype":"interrupt","cancel_queued":true}}),
                    )
                    .await;
                    process.stop().await;
                    return Ok(());
                }
            }
            Some("result") => {
                if frame["is_error"] == true {
                    let detail = frame["errors"]
                        .as_array()
                        .map(|es| {
                            es.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .or_else(|| frame["result"].as_str().map(str::to_owned))
                        .unwrap_or_else(|| "CLI returned an error result".into());
                    return Err(ApiError::upstream(
                        detail.chars().take(2000).collect::<String>(),
                    ));
                }
                return Err(ApiError::upstream(
                    "CLI returned result without a complete stream_event message; --include-partial-messages is required",
                ));
            }
            _ => {} // system, assistant snapshots, replay, keepalive, cancellation, etc.
        }
    }
}
struct Process {
    child: Child,
    stderr: Option<tokio::task::JoinHandle<()>>,
}
impl Process {
    async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        if let Some(t) = self.stderr.take() {
            t.abort();
        }
        // Child has kill_on_drop=true; Tokio reaps terminated children.
    }
}
async fn write(stdin: &mut ChildStdin, value: &Value) -> Result<()> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|_| ApiError::upstream("RPC encode failed"))?;
    bytes.push(b'\n');
    stdin
        .write_all(&bytes)
        .await
        .map_err(|_| ApiError::upstream("CLI stdin closed"))?;
    stdin
        .flush()
        .await
        .map_err(|_| ApiError::upstream("CLI stdin flush failed"))
}

pub fn control_reply(frame: &Value, req: &MessagesRequest) -> Result<Value> {
    let id = frame["request_id"]
        .as_str()
        .ok_or_else(|| ApiError::upstream("CLI control request has no request_id"))?;
    let r = &frame["request"];
    let payload = match r["subtype"].as_str() {
        Some("can_use_tool") => {
            json!({"behavior":"deny","message":"Tools are executed by the Messages API caller, never by this bridge", "toolUseID":r["tool_use_id"]})
        }
        Some("mcp_message") if r["server_name"] == MCP_SERVER => {
            let m = &r["message"];
            let mut reply = json!({"jsonrpc":"2.0","id":m.get("id").cloned().unwrap_or(json!(0))});
            match m["method"].as_str() {
                Some("initialize") => {
                    reply["result"] = json!({"protocolVersion":m["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                    "capabilities":{"tools":{}},"serverInfo":{"name":MCP_SERVER,"version":"0.1.0"}})
                }
                Some("tools/list") => reply["result"] = json!({"tools":req.mcp_tools()}),
                Some("notifications/initialized" | "ping") => reply["result"] = json!({}),
                Some("tools/call") => {
                    reply["error"] =
                        json!({"code":-32601,"message":"Tool execution belongs to the HTTP caller"})
                }
                _ => reply["error"] = json!({"code":-32601,"message":"Unsupported MCP method"}),
            }
            json!({"mcp_response":reply})
        }
        Some("request_user_dialog") => json!({"behavior":"cancelled"}),
        Some("elicitation") => json!({"action":"decline"}),
        _ => {
            return Ok(
                json!({"type":"control_response","response":{"subtype":"error","request_id":id,
            "error":"Unsupported control callback"}}),
            );
        }
    };
    Ok(
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":payload}}),
    )
}
