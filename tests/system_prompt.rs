use claude_messages_bridge::request::MessagesRequest;
use serde_json::{Value, json};

const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.";
const BILLING: &str =
    "x-anthropic-billing-header: cc_version=0.0.0.abc; cc_entrypoint=sdk-cli; cch=00000;";

fn parts(system: Value) -> Vec<String> {
    let request: MessagesRequest = serde_json::from_value(json!({
        "model":"test", "max_tokens":128, "system":system,
        "messages":[{"role":"user","content":"hello"}]
    }))
    .unwrap();
    request.validate().unwrap().system_parts().unwrap()
}

#[test]
fn detects_native_wrappers_in_text_and_blocks_without_reusing_cch() {
    let body = "Follow the caller's instructions.\n\n保持原文和空白。\n";
    assert_eq!(
        parts(json!(format!("{BILLING}\n\n{IDENTITY}\n\n{body}"))),
        vec![body]
    );
    assert_eq!(
        parts(json!([
            {"type":"text","text":BILLING},
            {"type":"text","text":IDENTITY},
            {"type":"text","text":body}
        ])),
        vec![body]
    );
    let repeated = format!("{BILLING}\n\n{IDENTITY}\n\n{IDENTITY}\n\n{body}");
    assert_eq!(parts(json!(repeated)), vec![body]);
}

#[test]
fn custom_text_and_quoted_or_unrecognized_prefixes_are_preserved() {
    for body in [
        "You are a helpful assistant.",
        &format!("Explain this example:\n\n{BILLING}\n\n{IDENTITY}"),
        &format!("{BILLING}\n\nThis is documentation, not a CLI wrapper."),
        "x-anthropic-billing-header: a custom field for discussion",
    ] {
        assert_eq!(parts(json!(body)), vec![body]);
    }
    assert_eq!(
        parts(json!([{"type":"text","text":"first"},{"type":"text","text":"second"}])),
        vec!["first", "second"]
    );
}

#[test]
fn empty_system_and_prefix_only_input_explicitly_clear_the_cli_body() {
    for input in [
        Value::Null,
        json!(""),
        json!([]),
        json!(IDENTITY),
        json!(format!("{BILLING}\n\n{IDENTITY}")),
    ] {
        assert_eq!(parts(input), vec![""]);
    }
}
