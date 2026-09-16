use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use claude_messages_bridge::{
    AppState,
    config::Config,
    router,
    store::{Credential, CredentialStore},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tower::ServiceExt;
const ADMIN: &str = "test-admin-token-with-at-least-32-bytes";
fn state(dir: &std::path::Path) -> AppState {
    let mut c = Config {
        admin_token: Some(ADMIN.into()),
        api_key: Some("messages-key".into()),
        credential_db_path: Some(dir.join("creds.redb")),
        cli: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli.py"),
        ..Config::default()
    };
    c.cli_env.insert(
        "FAKE_TRACE".into(),
        dir.join("rpc.jsonl").display().to_string(),
    );
    AppState::new(c)
}
fn req(method: &str, path: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        b = b.header("authorization", format!("Bearer {token}"))
    }
    b.body(Body::from(body.to_string())).unwrap()
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let r = app
        .clone()
        .oneshot(req(method, path, Some(ADMIN), body))
        .await
        .unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn idle(state: &AppState) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.permits.available_permits() != state.config.concurrency {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn terminal(app: &Router) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (_, v) = call(app, "GET", "/api/admin/oauth", Value::Null).await;
            if !matches!(
                v["login"]["status"].as_str(),
                Some("waiting" | "submitting" | "starting")
            ) {
                return v["login"].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn credential() -> Credential {
    Credential {
        access_token: "saved-access".into(),
        refresh_token: Some("saved-refresh".into()),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        scopes: vec!["user:inference".into()],
        api_key: None,
        method: "claudeai".into(),
        email: Some("saved@example.com".into()),
        organization: None,
        subscription: None,
        native_credentials: Value::Null,
        native_config: Value::Null,
    }
}
fn code(login: &Value, value: &str) -> Value {
    let url = url::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    json!({"code":format!("{value}#{state}")})
}
fn callback(login: &Value) -> String {
    format!("/api/admin/oauth/{}/code", login["id"].as_str().unwrap())
}
#[tokio::test]
async fn admin_auth_is_separate_and_responses_never_leak_credentials() {
    let d = tempfile::tempdir().unwrap();
    let state = state(d.path());
    state
        .oauth
        .credentials()
        .unwrap()
        .save(&credential())
        .unwrap();
    let app = router(state);
    for token in [None, Some("messages-key"), Some("bad")] {
        let r = app
            .clone()
            .oneshot(req("GET", "/api/admin/status", token, Value::Null))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(r.headers()["cache-control"], "no-store");
    }
    let (_, v) = call(&app, "GET", "/api/admin/status", Value::Null).await;
    assert_eq!(v["account"]["email"], "saved@example.com");
    assert!(!v.to_string().contains("saved-access"));
    assert!(!v.to_string().contains("saved-refresh"));
    assert_eq!(
        app.oneshot(req("POST", "/v1/messages", Some(ADMIN), json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        router(AppState::new(Config::default()))
            .oneshot(req(
                "POST",
                "/api/admin/oauth/start",
                Some(ADMIN),
                json!({})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
#[tokio::test]
async fn native_oauth_rpcs_state_single_use_redb_restart_logout_and_console() {
    let d = tempfile::tempdir().unwrap();
    let state1 = state(d.path());
    let app = router(state1.clone());
    let (s, login) = call(
        &app,
        "POST",
        "/api/admin/oauth/start",
        json!({"method":"claudeai"}),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(state1.permits.available_permits(), 0);
    assert_eq!(
        call(
            &app,
            "POST",
            &callback(&login),
            json!({"code":"ok#wrong-state"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "POST", "/api/admin/oauth/start", json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&app, "POST", "/api/admin/logout", json!({})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&app, "POST", &callback(&login), code(&login, "ok"))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(terminal(&app).await["status"], "succeeded");
    idle(&state1).await;
    assert_eq!(
        call(&app, "POST", &callback(&login), code(&login, "ok"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let trace: Vec<Value> = std::fs::read_to_string(d.path().join("rpc.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        trace
            .iter()
            .map(|v| v["request"]["subtype"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["initialize", "claude_authenticate", "claude_oauth_callback"]
    );
    assert!(trace[1]["request"]["loginWithClaudeAi"].as_bool().unwrap());
    assert!(trace[2]["request"].get("code_verifier").is_none());
    let saved = state1.oauth.credentials().unwrap().load().unwrap().unwrap();
    assert_eq!(saved.access_token, "secret-access");
    assert_eq!(
        saved.native_credentials["claudeAiOauth"]["clientId"],
        "cli-owned-public-client"
    );
    let cache = state1.oauth.inference_cache().await.unwrap().unwrap();
    let cache_path = cache.path().to_owned();
    drop(cache);
    drop(app);
    drop(state1);
    assert!(!cache_path.exists());
    let state2 = state(d.path());
    assert_eq!(
        state2
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .unwrap()
            .email
            .as_deref(),
        Some("sample@example.com")
    );
    let app = router(state2.clone());
    let cache = state2.oauth.inference_cache().await.unwrap().unwrap();
    let native: Value =
        serde_json::from_slice(&std::fs::read(cache.path().join(".credentials.json")).unwrap())
            .unwrap();
    assert_eq!(native["claudeAiOauth"]["refreshToken"], "secret-refresh");
    let cache_path = cache.path().to_owned();
    drop(cache);
    assert_eq!(
        call(&app, "POST", "/api/admin/logout", json!({})).await.0,
        StatusCode::OK
    );
    assert!(!cache_path.exists());
    assert!(
        state2
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .is_none()
    );
    let (_, login) = call(
        &app,
        "POST",
        "/api/admin/oauth/start",
        json!({"method":"console"}),
    )
    .await;
    call(&app, "POST", &callback(&login), code(&login, "console")).await;
    assert_eq!(terminal(&app).await["status"], "succeeded");
    idle(&state2).await;
    assert_eq!(
        state2
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .unwrap()
            .api_key
            .as_deref(),
        Some("secret-console-key")
    );
}
#[tokio::test]
async fn cancel_expire_rpc_failure_and_request_gate_preserve_previous_account() {
    let d = tempfile::tempdir().unwrap();
    let mut state = state(d.path());
    Arc::get_mut(&mut state.config).unwrap().oauth_timeout = Duration::from_millis(150);
    state
        .oauth
        .credentials()
        .unwrap()
        .save(&credential())
        .unwrap();
    let app = router(state.clone());
    let permit = state.permits.clone().acquire_owned().await.unwrap();
    assert_eq!(
        call(&app, "POST", "/api/admin/oauth/start", json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    drop(permit);
    let (_, login) = call(&app, "POST", "/api/admin/oauth/start", json!({})).await;
    call(&app, "POST", &callback(&login), code(&login, "bad")).await;
    let failed = terminal(&app).await;
    assert_eq!(failed["status"], "failed");
    assert!(!failed.to_string().contains("Do not expose"));
    idle(&state).await;
    call(&app, "POST", "/api/admin/oauth/start", json!({})).await;
    assert_eq!(terminal(&app).await["status"], "expired");
    idle(&state).await;
    let (_, login) = call(&app, "POST", "/api/admin/oauth/start", json!({})).await;
    call(&app, "POST", &callback(&login), code(&login, "hang")).await;
    call(
        &app,
        "DELETE",
        &format!("/api/admin/oauth/{}", login["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    idle(&state).await;
    assert_eq!(
        state
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .unwrap()
            .access_token,
        "saved-access"
    );
}
#[tokio::test]
async fn native_cli_refresh_is_written_to_redb_and_shared_cache_is_reused() {
    let d = tempfile::tempdir().unwrap();
    let mut state = state(d.path());
    state
        .oauth
        .credentials()
        .unwrap()
        .save(&credential())
        .unwrap();
    Arc::get_mut(&mut state.config)
        .unwrap()
        .cli_env
        .insert("FAKE_MODE".into(), "managed".into());
    let (a, b) = tokio::join!(state.oauth.inference_cache(), state.oauth.inference_cache());
    let a = a.unwrap().unwrap();
    let b = b.unwrap().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let app = router(state.clone());
    let r=app.oneshot(req("POST","/v1/messages",Some("messages-key"),json!({"model":"test-model","max_tokens":128,"messages":[{"role":"user","content":"hello"}]}))).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    idle(&state).await;
    let saved = state.oauth.credentials().unwrap().load().unwrap().unwrap();
    assert_eq!(saved.access_token, "refreshed-by-cli");
    assert_eq!(saved.refresh_token.as_deref(), Some("rotated-by-cli"));
}
#[test]
fn redb_file_permissions_and_failed_reopen_are_safe() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("credentials.redb");
    let store = CredentialStore::open(&path).unwrap();
    store.save(&credential()).unwrap();
    assert!(CredentialStore::open(&path).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    store.clear().unwrap();
    drop(store);
    assert!(
        CredentialStore::open(&path)
            .unwrap()
            .load()
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn unsupported_login_options_are_rejected_instead_of_implemented_in_rust() {
    let d = tempfile::tempdir().unwrap();
    let app = router(state(d.path()));
    for body in [json!({"email":"user@example.com"}), json!({"sso":true})] {
        let r = app
            .clone()
            .oneshot(req("POST", "/api/admin/oauth/start", Some(ADMIN), body))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
