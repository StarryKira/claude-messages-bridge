use crate::error::{ApiError, Result};
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default)]
pub struct MessageAccumulator {
    message: Option<Value>,
    blocks: Vec<Block>,
    done: bool,
}
struct Block {
    value: Value,
    partial_json: String,
    closed: bool,
}
impl MessageAccumulator {
    // Validate/reassemble the same events forwarded to SSE clients.
    pub fn push(&mut self, event: &Value) -> Result<()> {
        if self.done {
            return Err(ApiError::upstream(
                "CLI emitted an event after message_stop",
            ));
        }
        match event["type"].as_str() {
            Some("message_start") => {
                if self.message.is_some() {
                    return Err(ApiError::upstream("Unexpected second model message"));
                }
                let mut m = event["message"].clone();
                if m["type"] != "message"
                    || m["role"] != "assistant"
                    || !m["id"].is_string()
                    || !m["model"].is_string()
                    || !m["usage"].is_object()
                {
                    return Err(ApiError::upstream("Invalid CLI message_start"));
                }
                m["content"] = json!([]);
                m["stop_reason"] = Value::Null;
                m["stop_sequence"] = Value::Null;
                self.message = Some(m);
            }
            Some("content_block_start") => {
                self.started()?;
                let index = index(event)?;
                if index != self.blocks.len() {
                    return Err(ApiError::upstream("Non-contiguous content block index"));
                }
                let b = event["content_block"].clone();
                if !b.is_object() || !b["type"].is_string() {
                    return Err(ApiError::upstream("Invalid content block"));
                }
                self.blocks.push(Block {
                    value: b,
                    partial_json: String::new(),
                    closed: false,
                });
            }
            Some("content_block_delta") => {
                let b = self.block(event)?;
                let d = &event["delta"];
                match d["type"].as_str() {
                    Some("text_delta") if b.value["type"] == "text" => {
                        append(&mut b.value, "text", d, "text")?
                    }
                    Some("thinking_delta") if b.value["type"] == "thinking" => {
                        append(&mut b.value, "thinking", d, "thinking")?
                    }
                    Some("signature_delta") if b.value["type"] == "thinking" => {
                        append(&mut b.value, "signature", d, "signature")?
                    }
                    Some("input_json_delta") if b.value["type"] == "tool_use" => {
                        b.partial_json.push_str(
                            d["partial_json"]
                                .as_str()
                                .ok_or_else(|| ApiError::upstream("Invalid tool JSON delta"))?,
                        );
                    }
                    Some("citations_delta") if b.value["type"] == "text" => {
                        if b.value.get("citations").is_none() {
                            b.value["citations"] = json!([]);
                        }
                        b.value["citations"]
                            .as_array_mut()
                            .ok_or_else(|| ApiError::upstream("Invalid citations array"))?
                            .push(d["citation"].clone());
                    }
                    _ => {
                        return Err(ApiError::upstream(
                            "Unsupported or mismatched model content delta",
                        ));
                    }
                }
            }
            Some("content_block_stop") => {
                let b = self.block(event)?;
                if b.value["type"] == "tool_use" {
                    if !b.partial_json.is_empty() {
                        b.value["input"] = serde_json::from_str(&b.partial_json).map_err(|_| {
                            ApiError::upstream("CLI returned incomplete tool input JSON")
                        })?;
                    }
                    if !b.value["input"].is_object() {
                        return Err(ApiError::upstream("Tool input must be an object"));
                    }
                }
                b.closed = true;
            }
            Some("message_delta") => {
                self.started()?;
                let m = self.message.as_mut().unwrap();
                if let Some(delta) = event["delta"].as_object() {
                    for (key, value) in delta {
                        m[key] = value.clone();
                    }
                }
                if let Some(usage) = event["usage"].as_object() {
                    for (key, value) in usage {
                        m["usage"][key] = value.clone();
                    }
                }
            }
            Some("message_stop") => {
                self.started()?;
                if self.blocks.iter().any(|b| !b.closed)
                    || !self.message.as_ref().unwrap()["stop_reason"].is_string()
                {
                    return Err(ApiError::upstream("CLI ended an incomplete model message"));
                }
                self.message.as_mut().unwrap()["content"] =
                    Value::Array(self.blocks.iter().map(|b| b.value.clone()).collect());
                self.done = true;
            }
            Some("ping") => {}
            Some("error") => {
                return Err(ApiError::upstream(
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("CLI model stream failed"),
                ));
            }
            _ => {} // Anthropic may add non-content event types.
        }
        Ok(())
    }
    pub fn complete(&self) -> bool {
        self.done
    }
    pub fn finish(self) -> Result<Value> {
        if !self.done {
            return Err(ApiError::upstream("CLI stream ended before message_stop"));
        }
        self.message
            .ok_or_else(|| ApiError::upstream("CLI produced no model message"))
    }
    fn started(&self) -> Result<()> {
        if self.message.is_some() {
            Ok(())
        } else {
            Err(ApiError::upstream("CLI stream event before message_start"))
        }
    }
    fn block(&mut self, event: &Value) -> Result<&mut Block> {
        self.started()?;
        self.blocks
            .get_mut(index(event)?)
            .filter(|b| !b.closed)
            .ok_or_else(|| ApiError::upstream("Unknown or closed content block"))
    }
}
fn index(event: &Value) -> Result<usize> {
    event["index"]
        .as_u64()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| ApiError::upstream("Invalid content block index"))
}
fn append(target: &mut Value, key: &str, delta: &Value, field: &str) -> Result<()> {
    let value = delta[field]
        .as_str()
        .ok_or_else(|| ApiError::upstream("Invalid string delta"))?;
    let old = target.get(key).and_then(Value::as_str).unwrap_or("");
    target[key] = json!(format!("{old}{value}"));
    Ok(())
}
pub fn translate_event(event: &mut Value, tool_names: &HashMap<String, String>) -> Result<()> {
    if event["type"] == "content_block_start" && event["content_block"]["type"] == "tool_use" {
        let name = event["content_block"]["name"]
            .as_str()
            .ok_or_else(|| ApiError::upstream("Tool name missing"))?;
        let original = tool_names
            .get(name)
            .ok_or_else(|| ApiError::upstream("CLI returned a tool not declared by the caller"))?;
        event["content_block"]["name"] = json!(original);
    }
    Ok(())
}
