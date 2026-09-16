use claude_messages_bridge::{
    request::MessagesRequest, response::MessageAccumulator, rpc::control_reply,
};
use serde_json::{Value, json};

fn request(value: Value) -> MessagesRequest {
    serde_json::from_value(value).unwrap()
}
fn base() -> Value {
    json!({"model":"test","max_tokens":128,"messages":[{"role":"user","content":"Hi"}]})
}

#[test]
fn rejects_unsupported_parameters_and_incomplete_tool_roundtrips() {
    let mut value = base();
    value["temperature"] = json!(0.3);
    assert!(serde_json::from_value::<MessagesRequest>(value).is_err());
    let mut value = base();
    value["tool_choice"] = json!({"type":"any"});
    assert!(request(value).validate().is_err());
    let mut value = base();
    value["messages"] = json!([
        {"role":"user","content":"Hi"},
        {"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"weather","input":{}}]},
        {"role":"user","content":"continue"}]);
    assert!(request(value).validate().is_err());
    let mut value = base();
    value["messages"][0]["content"] =
        json!([{"type":"tool_result","tool_use_id":"missing","content":"result"}]);
    assert!(request(value).validate().is_err());
}
#[test]
fn adjacent_roles_merge_and_none_removes_tool_advertisement() {
    let mut value = base();
    value["messages"] = json!([{"role":"user","content":"a"},{"role":"user","content":"b"}]);
    value["tools"] = json!([{"name":"weather","input_schema":{"type":"object"}}]);
    value["tool_choice"] = json!({"type":"none"});
    let req = request(value).validate().unwrap();
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].content.as_array().unwrap().len(), 2);
    assert_eq!(req.mcp_tools(), json!([]));
}
#[test]
fn mcp_double_envelope_and_permission_denial_are_correct() {
    let req = request(base()).validate().unwrap();
    let reply=control_reply(&json!({"type":"control_request","request_id":"outer","request":{
        "subtype":"mcp_message","server_name":"messages","message":{"jsonrpc":"2.0","id":12,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}}}),&req).unwrap();
    assert_eq!(reply["response"]["request_id"], "outer");
    assert_eq!(reply["response"]["response"]["mcp_response"]["id"], 12);
    assert_eq!(
        reply["response"]["response"]["mcp_response"]["result"]["protocolVersion"],
        "2025-11-25"
    );
    let reply=control_reply(&json!({"type":"control_request","request_id":"deny","request":{"subtype":"can_use_tool","tool_use_id":"toolu_1"}}),&req).unwrap();
    assert_eq!(reply["response"]["subtype"], "success");
    assert_eq!(reply["response"]["response"]["behavior"], "deny");
    assert_eq!(reply["response"]["response"]["toolUseID"], "toolu_1");
}
#[test]
fn thinking_signatures_and_cumulative_usage_are_preserved() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"test","usage":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":3}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reasoning"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig1"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig2"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}),
        json!({"type":"message_stop"}),
    ];
    let mut acc = MessageAccumulator::default();
    for event in events {
        acc.push(&event).unwrap();
    }
    let m = acc.finish().unwrap();
    assert_eq!(m["content"][0]["signature"], "sig1sig2");
    assert_eq!(m["usage"]["output_tokens"], 6);
    assert_eq!(m["usage"]["cache_read_input_tokens"], 3);
}
#[test]
fn incomplete_or_out_of_order_stream_is_not_success() {
    let mut acc = MessageAccumulator::default();
    assert!(acc.push(&json!({"type":"message_stop"})).is_err());
    assert!(MessageAccumulator::default().finish().is_err());
}
