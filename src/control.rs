//! Native CLI control RPC used for account operations. No HTTP client lives here.
use crate::{
    config::Config,
    credential_cache::CredentialCache,
    error::{ApiError, Result},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tokio_util::codec::{FramedRead, LinesCodec};
use uuid::Uuid;

pub struct ControlClient {
    child: Child,
    stdin: ChildStdin,
    lines: FramedRead<ChildStdout, LinesCodec>,
    stderr: tokio::task::JoinHandle<()>,
    limit: usize,
}
impl ControlClient {
    pub async fn start(config: &Config, cache: &CredentialCache) -> Result<Self> {
        let mut command = Command::new(&config.cli);
        config.apply_cli_env(&mut command);
        cache.apply(&mut command)?;
        command
            .current_dir(cache.path())
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--tools",
                "",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--setting-sources",
                "",
                "--settings",
                "{\"disableAllHooks\":true}",
                "--no-session-persistence",
                "--no-chrome",
                "--disable-slash-commands",
            ])
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|_| ApiError::upstream("Cannot start CLI authentication RPC"))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let drain = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            lines: FramedRead::new(
                stdout,
                LinesCodec::new_with_max_length(config.max_frame_bytes),
            ),
            stderr: drain,
            limit: config.max_output_bytes,
        };
        tokio::time::timeout(config.init_timeout,client.call(json!({"subtype":"initialize","hooks":{},"sdkMcpServers":[],"supportedDialogKinds":[],"promptSuggestions":false}))).await.map_err(|_|ApiError::upstream("CLI authentication initialize timed out"))??;
        Ok(client)
    }
    pub async fn close(&mut self) {
        let _ = self.stdin.shutdown().await;
        if tokio::time::timeout(std::time::Duration::from_millis(300), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
    async fn write(&mut self, frame: Value) -> Result<()> {
        let mut bytes =
            serde_json::to_vec(&frame).map_err(|_| ApiError::upstream("RPC encoding failed"))?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|_| ApiError::upstream("CLI authentication stdin closed"))?;
        self.stdin
            .flush()
            .await
            .map_err(|_| ApiError::upstream("CLI authentication stdin closed"))
    }
    pub async fn call(&mut self, request: Value) -> Result<Value> {
        let id = Uuid::new_v4().to_string();
        let subtype = request["subtype"].as_str().unwrap_or("unknown").to_owned();
        self.write(json!({"type":"control_request","request_id":id,"request":request}))
            .await?;
        let mut bytes = 0usize;
        while let Some(line) = self.lines.next().await {
            let line =
                line.map_err(|_| ApiError::upstream("CLI authentication frame exceeded limit"))?;
            bytes = bytes.saturating_add(line.len());
            if bytes > self.limit {
                return Err(ApiError::upstream(
                    "CLI authentication output exceeded limit",
                ));
            }
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame["type"] == "control_response" && frame["response"]["request_id"] == id {
                if frame["response"]["subtype"] != "success" {
                    return Err(ApiError::upstream(format!(
                        "CLI rejected {subtype} RPC; check CLI version, account policy and authorization code"
                    )));
                }
                return Ok(frame["response"]["response"].clone());
            }
            if frame["type"] == "control_request" {
                let request_id = frame["request_id"].clone();
                let payload = match frame["request"]["subtype"].as_str() {
                    Some("can_use_tool") => Some(
                        json!({"behavior":"deny","message":"Account RPC cannot execute tools"}),
                    ),
                    Some("request_user_dialog") => Some(json!({"behavior":"cancelled"})),
                    Some("elicitation") => Some(json!({"action":"decline"})),
                    _ => None,
                };
                self.write(if let Some(payload)=payload{json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":payload}})}else{json!({"type":"control_response","response":{"subtype":"error","request_id":request_id,"error":"Unsupported authentication callback"}})}).await?;
            }
        }
        Err(ApiError::upstream(
            "CLI exited before authentication RPC completed",
        ))
    }
}
impl Drop for ControlClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.stderr.abort();
    }
}
