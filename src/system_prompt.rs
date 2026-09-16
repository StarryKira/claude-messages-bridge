//! Recognize CLI-owned leading wrappers without changing the caller's prompt body.
//! The CLI, not this adapter, generates the outgoing attribution and cch value.

const IDENTITIES: &[&str] = &[
    "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.",
    "You are Claude Code, Anthropic's official CLI for Claude.",
    "You are a Claude agent, built on Anthropic's Claude Agent SDK.",
];

/// CLI joins system blocks with a blank line. Only remove complete leading
/// wrapper paragraphs; quoted markers or instructions elsewhere stay untouched.
pub fn normalize(parts: Vec<String>) -> Vec<String> {
    let joined = parts.join("\n\n");
    let mut body = joined.as_str();
    loop {
        let (first, rest) = paragraph(body);
        if IDENTITIES.contains(&first) {
            body = rest;
            continue;
        }
        let (next, after) = paragraph(rest);
        if billing_header(first) && IDENTITIES.contains(&next) {
            body = after;
            continue;
        }
        break;
    }
    if body.len() == joined.len() && !parts.is_empty() {
        parts
    } else {
        vec![body.to_owned()]
    }
}

fn paragraph(value: &str) -> (&str, &str) {
    value.split_once("\n\n").unwrap_or((value, ""))
}

fn billing_header(value: &str) -> bool {
    let Some(fields) = value.strip_prefix("x-anthropic-billing-header: ") else {
        return false;
    };
    if fields.contains(['\n', '\r']) || !fields.ends_with(';') {
        return false;
    }
    let mut version = false;
    let mut entrypoint = false;
    for field in fields.split(';').filter(|field| !field.trim().is_empty()) {
        let Some((key, value)) = field.trim().split_once('=') else {
            return false;
        };
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return false;
        }
        version |= key == "cc_version";
        entrypoint |= key == "cc_entrypoint";
        if key != "cch" && !key.starts_with("cc_") {
            return false;
        }
    }
    version && entrypoint
}
