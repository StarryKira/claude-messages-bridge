//! Opt-in real CLI test; all inference traffic goes to a local fake Anthropic API.
use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    response::{Sse, sse::Event},
    routing::post,
};
use claude_messages_bridge::{AppState, config::Config, router, store::Credential};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;
#[tokio::test]
#[ignore = "Set REAL_CLAUDE_CLI to run against an installed CLI and local fake API"]
async fn real_cli_uses_access_token_from_redb() {
    let cli = std::env::var("REAL_CLAUDE_CLI").expect("Set REAL_CLAUDE_CLI");
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let api=Router::new().route("/v1/messages",post(move |headers:HeaderMap,Json(body):Json<Value>|{let seen=seen.clone();async move{
        assert_eq!(headers["authorization"],"Bearer sk-ant-oat01-local-dummy-token");
        assert_eq!(body["max_tokens"],128);seen.fetch_add(1,Ordering::SeqCst);
        let events=vec![
            json!({"type":"message_start","message":{"id":"msg_redb_cli","type":"message","role":"assistant","model":"claude-sonnet-4-6","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":4,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"redb credential works"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":4}}),
            json!({"type":"message_stop"}),
        ];
        Sse::new(futures_util::stream::iter(events.into_iter().map(|v|Ok::<_,Infallible>(Event::default().event(v["type"].as_str().unwrap()).json_data(v).unwrap()))))
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        cli: cli.into(),
        credential_db_path: Some(dir.path().join("credentials.redb")),
        request_timeout: Duration::from_secs(30),
        ..Config::default()
    };
    config
        .cli_env
        .insert("HOME".into(), dir.path().display().to_string());
    config.cli_env.insert("ANTHROPIC_BASE_URL".into(), base);
    config
        .cli_env
        .insert("ANTHROPIC_API_KEY".into(), "must-not-be-used".into());
    let state = AppState::new(config);
    state
        .oauth
        .credentials()
        .unwrap()
        .save(&Credential {
            access_token: "sk-ant-oat01-local-dummy-token".into(),
            refresh_token: Some("dummy-native-refresh".into()),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            scopes: vec!["user:inference".into()],
            api_key: None,
            method: "claudeai".into(),
            email: None,
            organization: None,
            subscription: None,
            native_credentials: Value::Null,
            native_config: json!({"oauthAccount":{"accountUuid":"11111111-1111-4111-8111-111111111111","emailAddress":"test@example.com"}}),
        })
        .unwrap();
    let app = router(state);
    let request=Request::post("/v1/messages").header("content-type","application/json").body(Body::from(json!({"model":"claude-sonnet-4-6","max_tokens":128,"messages":[{"role":"user","content":"hello"}]}).to_string())).unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["content"][0]["text"], "redb credential works");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!dir.path().join(".claude/.credentials.json").exists());
    server.abort();
}
