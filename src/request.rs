use crate::error::{ApiError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub const MCP_SERVER: &str = "messages";
pub const MCP_PREFIX: &str = "mcp__messages__";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<InputMessage>,
    #[serde(default)]
    pub stream: bool,
    pub system: Option<Value>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    pub tool_choice: Option<Value>,
    pub thinking: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputMessage {
    pub role: String,
    pub content: Value,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

impl MessagesRequest {
    pub fn validate(mut self) -> Result<Self> {
        if self.model.trim().is_empty() || self.model.len() > 200 {
            return Err(ApiError::invalid(
                "model must be a nonempty string of at most 200 bytes",
            ));
        }
        if self.max_tokens == 0 {
            return Err(ApiError::invalid(
                "This CLI adapter requires max_tokens > 0; cache-only requests are unsupported",
            ));
        }
        if self.messages.is_empty() || self.messages.len() > 100_000 {
            return Err(ApiError::invalid("messages must contain 1..100000 entries"));
        }
        if self.messages.first().unwrap().role != "user"
            || self.messages.last().unwrap().role != "user"
        {
            return Err(ApiError::invalid(
                "This CLI adapter requires the first and last message to be user; assistant prefill is unsupported",
            ));
        }
        let mut names = HashSet::new();
        for t in &self.tools {
            if !valid_name(&t.name) || !names.insert(t.name.clone()) {
                return Err(ApiError::invalid(
                    "Tool names must be unique, 1..64 ASCII letters, digits, underscores or hyphens",
                ));
            }
            if !t.input_schema.is_object() || t.input_schema["type"] != "object" {
                return Err(ApiError::invalid(
                    "Each tool input_schema must be a JSON object schema with type=object",
                ));
            }
        }
        if let Some(choice) = &self.tool_choice {
            only_keys(choice, &["type"], "tool_choice")?;
            if !matches!(choice["type"].as_str(), Some("auto" | "none")) {
                return Err(ApiError::invalid(
                    "The CLI RPC supports tool_choice auto or none here; any/tool/disable_parallel_tool_use cannot be faithfully enforced",
                ));
            }
        }
        self.system_parts()?;
        if let Some(thinking) = &self.thinking {
            only_keys(thinking, &["type", "budget_tokens"], "thinking")?;
            match thinking["type"].as_str() {
                Some("disabled" | "adaptive") if thinking.get("budget_tokens").is_none() => {}
                Some("enabled")
                    if thinking["budget_tokens"]
                        .as_u64()
                        .is_some_and(|n| n >= 1024 && n < self.max_tokens as u64) => {}
                _ => {
                    return Err(ApiError::invalid(
                        "thinking must be disabled/adaptive, or enabled with 1024 <= budget_tokens < max_tokens",
                    ));
                }
            }
        }
        // Normalize adjacent roles without converting the conversation into a prompt string.
        let mut merged: Vec<InputMessage> = Vec::new();
        for mut m in self.messages {
            if !matches!(m.role.as_str(), "user" | "assistant") {
                return Err(ApiError::invalid("message role must be user or assistant"));
            }
            m.content = Value::Array(blocks(&m.content)?);
            for b in m.content.as_array().unwrap() {
                validate_block(b, &m.role)?;
            }
            if let Some(last) = merged.last_mut().filter(|last| last.role == m.role) {
                last.content
                    .as_array_mut()
                    .unwrap()
                    .extend(m.content.as_array().unwrap().iter().cloned());
            } else {
                merged.push(m);
            }
        }
        let mut pending: HashSet<String> = HashSet::new();
        let mut all_ids = HashSet::new();
        for m in &merged {
            let bs = m.content.as_array().unwrap();
            if m.role == "assistant" {
                if !pending.is_empty() {
                    return Err(ApiError::invalid(
                        "Missing tool_result for preceding tool_use",
                    ));
                }
                for b in bs.iter().filter(|b| b["type"] == "tool_use") {
                    let id = b["id"].as_str().unwrap().to_owned();
                    if !all_ids.insert(id.clone()) {
                        return Err(ApiError::invalid("Duplicate tool_use id in history"));
                    }
                    pending.insert(id);
                }
            } else {
                let mut non_result_seen = false;
                for b in bs {
                    if b["type"] == "tool_result" {
                        if non_result_seen {
                            return Err(ApiError::invalid(
                                "tool_result blocks must precede other user content",
                            ));
                        }
                        if !pending.remove(b["tool_use_id"].as_str().unwrap()) {
                            return Err(ApiError::invalid(
                                "tool_result must match an unresolved tool_use in the previous assistant message",
                            ));
                        }
                    } else {
                        non_result_seen = true;
                    }
                }
                if !pending.is_empty() {
                    return Err(ApiError::invalid(
                        "Provide a tool_result for every tool_use from the previous assistant message",
                    ));
                }
            }
        }
        self.messages = merged;
        Ok(self)
    }
    pub fn system_parts(&self) -> Result<Vec<String>> {
        let parts = match &self.system {
            None | Some(Value::Null) => Ok(vec![String::new()]), // Explicitly replace CLI's default coding prompt.
            Some(Value::String(s)) => Ok(vec![s.clone()]),
            Some(Value::Array(bs)) => bs
                .iter()
                .map(|b| {
                    only_keys(b, &["type", "text"], "system block")?;
                    if b["type"] != "text" {
                        return Err(ApiError::invalid("system supports text blocks only"));
                    }
                    string(b, "text").map(str::to_owned)
                })
                .collect(),
            _ => Err(ApiError::invalid(
                "system must be a string or array of text blocks",
            )),
        }?;
        Ok(crate::system_prompt::normalize(parts))
    }
    pub fn active_tools(&self) -> &[Tool] {
        if self
            .tool_choice
            .as_ref()
            .is_some_and(|c| c["type"] == "none")
        {
            &[]
        } else {
            &self.tools
        }
    }
    pub fn mcp_tools(&self) -> Value {
        Value::Array(self.active_tools().iter().map(|t| json!({
            "name":t.name,"description":t.description.as_deref().unwrap_or(""),"inputSchema":t.input_schema
        })).collect())
    }
    pub fn wire_content(&self, content: &Value) -> Value {
        let mut content = content.clone();
        if let Some(bs) = content.as_array_mut() {
            for b in bs {
                if b["type"] == "tool_use"
                    && let Some(name) = b["name"].as_str()
                {
                    b["name"] = json!(format!("{MCP_PREFIX}{name}"));
                }
            }
        }
        content
    }
    pub fn tool_map(&self) -> HashMap<String, String> {
        self.active_tools()
            .iter()
            .map(|t| (format!("{MCP_PREFIX}{}", t.name), t.name.clone()))
            .collect()
    }
}
fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn blocks(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::String(s) if !s.is_empty() => Ok(vec![json!({"type":"text","text":s})]),
        Value::Array(a) if !a.is_empty() => Ok(a.clone()),
        _ => Err(ApiError::invalid(
            "message content must be a nonempty string or nonempty block array",
        )),
    }
}
fn only_keys(v: &Value, keys: &[&str], context: &str) -> Result<()> {
    let obj = v
        .as_object()
        .ok_or_else(|| ApiError::invalid(format!("{context} must be an object")))?;
    if let Some(k) = obj.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(ApiError::invalid(format!(
            "Unsupported {context} field: {k}"
        )));
    }
    Ok(())
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| ApiError::invalid(format!("{key} must be a string")))
}
fn nonempty(v: &Value, key: &str) -> Result<()> {
    if string(v, key)?.is_empty() {
        Err(ApiError::invalid(format!("{key} must not be empty")))
    } else {
        Ok(())
    }
}
fn validate_block(b: &Value, role: &str) -> Result<()> {
    match b["type"].as_str() {
        Some("text") => {
            only_keys(b, &["type", "text"], "text block")?;
            string(b, "text")?;
        }
        Some("image") if role == "user" => {
            only_keys(b, &["type", "source"], "image block")?;
            let s = &b["source"];
            only_keys(s, &["type", "media_type", "data"], "image source")?;
            if s["type"] != "base64"
                || !matches!(
                    s["media_type"].as_str(),
                    Some("image/png" | "image/jpeg" | "image/gif" | "image/webp")
                )
            {
                return Err(ApiError::invalid(
                    "Images require a base64 PNG/JPEG/GIF/WebP source",
                ));
            }
            nonempty(s, "data")?;
        }
        Some("tool_use") if role == "assistant" => {
            only_keys(b, &["type", "id", "name", "input"], "tool_use block")?;
            nonempty(b, "id")?;
            if !valid_name(string(b, "name")?) || !b["input"].is_object() {
                return Err(ApiError::invalid(
                    "tool_use requires a valid name and object input",
                ));
            }
        }
        Some("tool_result") if role == "user" => {
            only_keys(
                b,
                &["type", "tool_use_id", "content", "is_error"],
                "tool_result block",
            )?;
            nonempty(b, "tool_use_id")?;
            if b.get("is_error").is_some_and(|v| !v.is_boolean()) {
                return Err(ApiError::invalid("is_error must be boolean"));
            }
            if let Some(content) = b.get("content") {
                match content {
                    Value::String(_) => {}
                    Value::Array(bs) => {
                        for b in bs {
                            if !matches!(b["type"].as_str(), Some("text" | "image")) {
                                return Err(ApiError::invalid(
                                    "tool_result content supports text/image blocks",
                                ));
                            }
                            validate_block(b, "user")?;
                        }
                    }
                    _ => {
                        return Err(ApiError::invalid(
                            "tool_result content must be a string or array",
                        ));
                    }
                }
            }
        }
        Some("thinking") if role == "assistant" => {
            only_keys(b, &["type", "thinking", "signature"], "thinking block")?;
            string(b, "thinking")?;
            nonempty(b, "signature")?;
        }
        Some("redacted_thinking") if role == "assistant" => {
            only_keys(b, &["type", "data"], "redacted_thinking block")?;
            nonempty(b, "data")?;
        }
        _ => {
            return Err(ApiError::invalid(format!(
                "Unsupported content block {:?} for role {role}",
                b["type"]
            )));
        }
    }
    Ok(())
}
