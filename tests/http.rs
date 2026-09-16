#![cfg(unix)]
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use claude_messages_bridge::{AppState, config::Config, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tower::ServiceExt;

fn config(mode: &str) -> Config {
    let mut c = Config {
        cli: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli.py"),
        request_timeout: Duration::from_secs(5),
        ..Config::default()
    };
    c.cli_env.insert("FAKE_MODE".into(), mode.into());
    c
}
fn body(stream: bool) -> Value {
    json!({"model":"test-model","max_tokens":128,"stream":stream,"messages":[{"role":"user","content":"Hi"}]})
}
fn tools(mut v: Value) -> Value {
    v["tools"] = json!([{"name":"weather","description":"Weather","input_schema":{"type":"object","properties":{"city":{"type":"string"}}}}]);
    v
}
fn req(v: Value) -> Request<Body> {
    Request::post("/v1/messages")
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(v.to_string()))
        .unwrap()
}
async fn json_response(r: Response) -> Value {
    serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
async fn text_response(r: Response) -> String {
    String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

#[tokio::test]
async fn nonstream_response_is_one_message_with_exact_usage() {
    let r = router(AppState::new(config("text")))
        .oneshot(req(body(false)))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.headers().contains_key("request-id"));
    let m = json_response(r).await;
    assert_eq!(m["type"], "message");
    assert_eq!(m["content"], json!([{"type":"text","text":"你好 bridge"}]));
    assert_eq!(m["stop_reason"], "end_turn");
    assert_eq!(m["usage"]["output_tokens"], 7);
    assert_eq!(m["usage"]["cache_read_input_tokens"], 5);
}
#[tokio::test]
async fn tool_use_and_sse_hide_mcp_prefix_and_never_emit_done_sentinel() {
    let app = router(AppState::new(config("tool")));
    let r = app.clone().oneshot(req(tools(body(false)))).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let m = json_response(r).await;
    assert_eq!(
        m["content"][0],
        json!({"type":"tool_use","id":"toolu_fixture","name":"weather","input":{"city":"Paris"}})
    );
    assert_eq!(m["stop_reason"], "tool_use");
    let r = app.oneshot(req(tools(body(true)))).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    let s = text_response(r).await;
    assert!(s.contains("event: message_start"));
    assert!(s.contains("event: message_stop"));
    assert!(s.contains("input_json_delta"));
    assert!(!s.contains("mcp__"));
    assert!(!s.contains("[DONE]"));
}
#[tokio::test]
async fn history_roundtrip_preserves_roles_tool_ids_and_names() {
    let mut b = tools(body(false));
    b["messages"] = json!([
        {"role":"user","content":"Earlier"},
        {"role":"assistant","content":[{"type":"tool_use","id":"toolu_prior","name":"weather","input":{}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_prior","content":"sunny"}]}]);
    let r = router(AppState::new(config("history")))
        .oneshot(req(b))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "{}", text_response(r).await);
}
#[tokio::test]
async fn errors_before_headers_use_http_errors_and_midstream_uses_sse_error() {
    for mode in ["init_error", "error_before", "crash"] {
        let r = router(AppState::new(config(mode)))
            .oneshot(req(body(false)))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY, "{mode}");
        assert_eq!(json_response(r).await["type"], "error");
    }
    let r = router(AppState::new(config("error_mid")))
        .oneshot(req(body(true)))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let s = text_response(r).await;
    assert!(s.contains("event: error"));
    assert!(!s.contains("event: message_stop"));
    let r = router(AppState::new(config("bad_tool_json")))
        .oneshot(req(tools(body(false))))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
}
#[tokio::test]
async fn init_timeout_request_timeout_and_frame_limit_are_bounded() {
    let mut c = config("init_hang");
    c.init_timeout = Duration::from_millis(150);
    let r = router(AppState::new(c))
        .oneshot(req(body(false)))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    let mut c = config("hang");
    c.request_timeout = Duration::from_millis(250);
    let r = router(AppState::new(c))
        .oneshot(req(body(false)))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::GATEWAY_TIMEOUT);
    let mut c = config("max_frame");
    c.max_frame_bytes = 1024;
    let r = router(AppState::new(c))
        .oneshot(req(body(false)))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
}
#[tokio::test]
async fn validates_auth_parameters_body_limit_and_concurrency() {
    let mut c = config("text");
    c.api_key = Some("secret".into());
    let app = router(AppState::new(c));
    let r = app.clone().oneshot(req(body(false))).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let mut q = req(body(false));
    q.headers_mut()
        .insert("x-api-key", "secret".parse().unwrap());
    assert_eq!(app.oneshot(q).await.unwrap().status(), StatusCode::OK);
    let mut b = body(false);
    b["temperature"] = json!(0.2);
    let r = router(AppState::new(config("text")))
        .oneshot(req(b))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let mut c = config("text");
    c.max_body_bytes = 16;
    let r = router(AppState::new(c))
        .oneshot(req(body(false)))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let state = AppState::new(config("text"));
    let _permit = state.permits.clone().acquire_many_owned(4).await.unwrap();
    let r = router(state).oneshot(req(body(false))).await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}
#[tokio::test]
async fn dropping_sse_kills_child_and_releases_permit() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut c = config("hang");
    c.cli_env
        .insert("FAKE_PID".into(), pidfile.display().to_string());
    let state = AppState::new(c);
    let permits = state.permits.clone();
    let r = router(state).oneshot(req(body(true))).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let pid = std::fs::read_to_string(pidfile).unwrap();
    drop(r);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let alive = tokio::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .unwrap()
                .success();
            if !alive && permits.available_permits() == 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("CLI process and permit leaked after client disconnect");
}

#[tokio::test]
async fn changed_system_prompt_is_injected_for_each_request_with_snapshots_disabled() {
    for (stream, text) in [(false, "first prompt"), (true, "different prompt")] {
        let mut cfg = config("text");
        cfg.cli_env
            .insert("FAKE_EXPECT_SYSTEM".into(), json!([text]).to_string());
        // A parent opt-out must not suppress the requested CLI attribution injection.
        cfg.cli_env
            .insert("CLAUDE_CODE_ATTRIBUTION_HEADER".into(), "0".into());
        let mut payload = body(stream);
        payload["system"] = json!([
            {"type":"text","text":"x-anthropic-billing-header: cc_version=0.0.0.abc; cc_entrypoint=sdk-cli; cch=00000;"},
            {"type":"text","text":"You are a Claude agent, built on Anthropic's Claude Agent SDK."},
            {"type":"text","text":text}
        ]);
        let response = router(AppState::new(cfg))
            .oneshot(req(payload))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content = text_response(response).await;
        assert!(!content.contains("event: error"), "{content}");
        if stream {
            assert!(content.contains("event: message_stop"));
        }
    }
}
