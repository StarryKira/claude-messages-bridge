use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    routing::{get, post},
};
use claude_messages_bridge::{
    AppState,
    config::Config,
    oauth::{CLIENT_ID, Endpoints, OAuthClient, REDIRECT_URI},
    router,
    store::{Credential, CredentialStore},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;
use tower::ServiceExt;
const ADMIN: &str = "test-admin-token-with-at-least-32-bytes";
#[derive(Clone, Default)]
struct MockOAuth {
    calls: Arc<Mutex<Vec<Value>>>,
    refreshes: Arc<AtomicUsize>,
}
async fn tokens(
    State(mock): State<MockOAuth>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    mock.calls.lock().await.push(body.clone());
    if body["code"] == "bad" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Do not expose this secret"})),
        );
    }
    let refresh = body["grant_type"] == "refresh_token";
    if refresh {
        mock.refreshes.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let scope = if body["code"] == "console" {
        "org:create_api_key user:profile"
    } else {
        "user:inference user:profile"
    };
    (
        StatusCode::OK,
        Json(
            json!({"access_token":if refresh {"refreshed-secret-access"} else {"secret-access"},"refresh_token":"rotated-secret-refresh","expires_in":3600,"scope":scope,"account":{"email_address":"sample@example.com"}}),
        ),
    )
}
async fn fake() -> (MockOAuth, Endpoints, tokio::task::JoinHandle<()>) {
    let mock = MockOAuth::default();
    let app = Router::new().route("/token",post(tokens)).route("/profile",get(||async{Json(json!({"account":{"email":"sample@example.com"},"organization":{"name":"Example Org","organization_type":"claude_pro"}}))})).route("/api-key",post(||async{Json(json!({"raw_key":"secret-console-key"}))})).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        mock,
        Endpoints {
            token: format!("{base}/token"),
            profile: format!("{base}/profile"),
            api_key: format!("{base}/api-key"),
        },
        handle,
    )
}
fn state(dir: &std::path::Path, endpoints: Endpoints) -> AppState {
    let c = Config {
        admin_token: Some(ADMIN.into()),
        api_key: Some("messages-key".into()),
        credential_db_path: Some(dir.join("creds.redb")),
        cli: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli.py"),
        ..Config::default()
    };
    let mut state = AppState::new(c);
    state.oauth = Arc::new(OAuthClient::new(state.oauth.store.clone(), endpoints).unwrap());
    state
}
fn req(method: &str, path: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let r = app
        .clone()
        .oneshot(req(method, path, Some(ADMIN), body))
        .await
        .unwrap();
    let s = r.status();
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&b).unwrap())
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
    }
}
#[tokio::test]
async fn admin_auth_is_separate_and_responses_never_leak_credentials() {
    let d = tempfile::tempdir().unwrap();
    let state = state(d.path(), Endpoints::default());
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
    let (_, body) = call(&app, "GET", "/api/admin/status", Value::Null).await;
    assert_eq!(body["account"]["email"], "saved@example.com");
    assert!(!body.to_string().contains("saved-access"));
    assert!(!body.to_string().contains("saved-refresh"));
    let r = app
        .oneshot(req("POST", "/v1/messages", Some(ADMIN), json!({})))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = router(AppState::new(Config::default()))
        .oneshot(req(
            "POST",
            "/api/admin/oauth/start",
            Some(ADMIN),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
}
#[tokio::test]
async fn oauth_pkce_state_single_use_redb_restart_logout_and_console_key() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let (mock, endpoints, server) = fake().await;
    let d = tempfile::tempdir().unwrap();
    let state = state(d.path(), endpoints);
    let app = router(state.clone());
    let (s, login) = call(
        &app,
        "POST",
        "/api/admin/oauth/start",
        json!({"email":"user@example.com","sso":true}),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(state.permits.available_permits(), 0);
    let url = url::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.host_str(), Some("claude.com"));
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(params["redirect_uri"], REDIRECT_URI);
    assert_eq!(params["login_method"], "sso");
    let path = format!("/api/admin/oauth/{}/code", login["id"].as_str().unwrap());
    assert_eq!(
        call(&app, "POST", &path, json!({"code":"ok#wrong-state"}))
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
        call(
            &app,
            "POST",
            &path,
            json!({"code":format!("ok#{}",params["state"])})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(terminal(&app).await["status"], "succeeded");
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            json!({"code":format!("ok#{}",params["state"])})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let calls = mock.calls.lock().await;
    let verifier = calls[0]["code_verifier"].as_str().unwrap();
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes())),
        params["code_challenge"]
    );
    drop(calls);
    assert_eq!(
        state
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .unwrap()
            .access_token,
        "secret-access"
    );
    drop(app);
    drop(state);
    let store = CredentialStore::open(&d.path().join("creds.redb")).unwrap();
    assert_eq!(
        store.load().unwrap().unwrap().email.as_deref(),
        Some("sample@example.com")
    );
    drop(store);
    let (_, endpoints2, server2) = fake().await;
    let state = state_for_restart(d.path(), endpoints2);
    let app = router(state.clone());
    assert_eq!(
        call(&app, "GET", "/api/admin/status", Value::Null).await.1["account"]["logged_in"],
        true
    );
    assert_eq!(
        call(&app, "POST", "/api/admin/logout", json!({})).await.0,
        StatusCode::OK
    );
    assert!(state.oauth.credentials().unwrap().load().unwrap().is_none());
    let (_, login) = call(
        &app,
        "POST",
        "/api/admin/oauth/start",
        json!({"method":"console"}),
    )
    .await;
    let url = url::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    let st = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let path = format!("/api/admin/oauth/{}/code", login["id"].as_str().unwrap());
    call(&app, "POST", &path, json!({"code":format!("console#{st}")})).await;
    assert_eq!(terminal(&app).await["status"], "succeeded");
    assert_eq!(
        state
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
    server.abort();
    server2.abort();
}
fn state_for_restart(dir: &std::path::Path, e: Endpoints) -> AppState {
    state(dir, e)
}
#[tokio::test]
async fn token_refresh_is_single_flight_and_persisted() {
    let (mock, endpoints, server) = fake().await;
    let d = tempfile::tempdir().unwrap();
    let state = state(d.path(), endpoints);
    let mut c = credential();
    c.expires_at = 0;
    state.oauth.credentials().unwrap().save(&c).unwrap();
    let (a, b) = tokio::join!(
        state.oauth.inference_credential(),
        state.oauth.inference_credential()
    );
    assert_eq!(a.unwrap().unwrap().access_token, "refreshed-secret-access");
    assert_eq!(b.unwrap().unwrap().access_token, "refreshed-secret-access");
    assert_eq!(mock.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        state
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("rotated-secret-refresh")
    );
    server.abort();
}
#[tokio::test]
async fn cancel_expire_provider_failure_and_request_gate_preserve_previous_account() {
    let (_, endpoints, server) = fake().await;
    let d = tempfile::tempdir().unwrap();
    let mut state = state(d.path(), endpoints);
    Arc::get_mut(&mut state.config).unwrap().oauth_timeout = Duration::from_millis(200);
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
    let url = url::Url::parse(login["authorization_url"].as_str().unwrap()).unwrap();
    let st = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let path = format!("/api/admin/oauth/{}/code", login["id"].as_str().unwrap());
    call(&app, "POST", &path, json!({"code":format!("bad#{st}")})).await;
    let failed = terminal(&app).await;
    assert_eq!(failed["status"], "failed");
    assert!(!failed.to_string().contains("Do not expose"));
    call(&app, "POST", "/api/admin/oauth/start", json!({})).await;
    assert_eq!(terminal(&app).await["status"], "expired");
    let (_, login) = call(&app, "POST", "/api/admin/oauth/start", json!({})).await;
    call(
        &app,
        "DELETE",
        &format!("/api/admin/oauth/{}", login["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while state.permits.available_permits() != 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
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
    server.abort();
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
    assert!(!d.path().join(".credentials.json").exists());
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
async fn redb_credential_is_used_by_rpc_without_exposing_refresh_token() {
    let d = tempfile::tempdir().unwrap();
    let mut state = state(d.path(), Endpoints::default());
    state
        .oauth
        .credentials()
        .unwrap()
        .save(&credential())
        .unwrap();
    let config = Arc::get_mut(&mut state.config).unwrap();
    config.cli_env.insert("FAKE_MODE".into(), "managed".into());
    config
        .cli_env
        .insert("ANTHROPIC_API_KEY".into(), "must-not-reach-cli".into());
    let app = router(state);
    let r=app.oneshot(req("POST","/v1/messages",Some("messages-key"),json!({"model":"test-model","max_tokens":128,"messages":[{"role":"user","content":"hello"}]}))).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}
