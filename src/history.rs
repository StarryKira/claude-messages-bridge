use crate::{
    error::{ApiError, Result},
    request::MessagesRequest,
};
use serde_json::{Value, json};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

// CLI-native transcript records preserve roles, content blocks and tool_use IDs.
// Each HTTP request owns its own temp directory; no global session lookup or writes.
pub async fn seed_history(
    req: &MessagesRequest,
    directory: &Path,
    session_id: &str,
) -> Result<Option<(std::path::PathBuf, String)>> {
    if req.messages.len() == 1 {
        return Ok(None);
    }
    let path = directory.join("history.jsonl");
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&path)
        .await
        .map_err(|_| ApiError::upstream("Cannot create temporary history"))?;
    let mut parent: Option<String> = None;
    // Seed complete tool pairs so resume validation cannot discard an unresolved
    // tool_use. --resume-session-at then trims back to the preceding assistant;
    // the final user turn is sent over stdin exactly once after initialize.
    let mut resume_at = String::new();
    for m in &req.messages {
        let id = Uuid::new_v4().to_string();
        let mut message = json!({"role":m.role,"content":req.wire_content(&m.content)});
        if m.role == "assistant" {
            resume_at = id.clone();
            let tool = m
                .content
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["type"] == "tool_use");
            message["id"] = json!(format!("msg_{}", Uuid::new_v4().simple()));
            message["type"] = json!("message");
            message["model"] = json!(req.model);
            message["stop_reason"] = json!(if tool { "tool_use" } else { "end_turn" });
            message["stop_sequence"] = Value::Null;
            message["usage"] = json!({"input_tokens":0,"output_tokens":0});
        }
        let row = json!({"type":m.role,"uuid":id,"parentUuid":parent,"sessionId":session_id,
            "isSidechain":false,"userType":"external","cwd":directory,"version":"2.1.272",
            "timestamp":chrono::Utc::now().to_rfc3339(),"message":message});
        let mut bytes =
            serde_json::to_vec(&row).map_err(|_| ApiError::upstream("Cannot encode history"))?;
        bytes.push(b'\n');
        file.write_all(&bytes)
            .await
            .map_err(|_| ApiError::upstream("Cannot write temporary history"))?;
        parent = Some(id);
    }
    file.flush()
        .await
        .map_err(|_| ApiError::upstream("Cannot flush temporary history"))?;
    Ok(Some((path, resume_at)))
}
