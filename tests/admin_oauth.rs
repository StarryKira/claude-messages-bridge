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
        native_config: json!({"oauthAccount":{"accountUuid":"fixture-id","emailAddress":"saved@example.com"}}),
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

async fn login_with(app: &Router, state: &AppState, value: &str) -> Value {
    let (status, login) = call(app, "POST", "/api/admin/oauth/start", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{login}");
    assert_eq!(
        call(app, "POST", &callback(&login), code(&login, value))
            .await
            .0,
        StatusCode::OK
    );
    let result = terminal(app).await;
    idle(state).await;
    result
}

#[tokio::test]
async fn instance_binding_rejects_other_accounts_even_after_logout_and_restart() {
    let d = tempfile::tempdir().unwrap();
    let first = state(d.path());
    let app = router(first.clone());
    assert_eq!(first.oauth.status().unwrap()["binding"], Value::Null);
    assert_eq!(
        login_with(&app, &first, "missing-identity").await["status"],
        "failed"
    );
    assert_eq!(first.oauth.status().unwrap()["binding"], Value::Null);
    assert!(first.oauth.credentials().unwrap().load().unwrap().is_none());

    assert_eq!(login_with(&app, &first, "ok").await["status"], "succeeded");
    let original = first.oauth.inference_cache().await.unwrap().unwrap();
    // The other account deliberately has the same email: the native UUID wins.
    let failed = login_with(&app, &first, "other").await;
    assert_eq!(failed["status"], "failed");
    assert!(
        failed["message"]
            .as_str()
            .unwrap()
            .contains("bound to another account")
    );
    let active = first.oauth.inference_cache().await.unwrap().unwrap();
    assert!(Arc::ptr_eq(&original, &active));
    assert_eq!(
        first.oauth.status().unwrap()["binding"]["account_id"],
        "fixture-id"
    );
    assert_eq!(
        first
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .unwrap()
            .access_token,
        "secret-access"
    );
    drop(active);
    drop(original);

    assert_eq!(
        call(&app, "POST", "/api/admin/logout", json!({})).await.0,
        StatusCode::OK
    );
    assert!(
        !first.oauth.status().unwrap()["logged_in"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        first.oauth.status().unwrap()["binding"]["account_id"],
        "fixture-id"
    );
    drop(app);
    drop(first);
    let restarted = state(d.path());
    let app = router(restarted.clone());
    assert_eq!(
        login_with(&app, &restarted, "other").await["status"],
        "failed"
    );
    assert!(
        restarted
            .oauth
            .credentials()
            .unwrap()
            .load()
            .unwrap()
            .is_none()
    );
    assert!(restarted.oauth.inference_cache().await.is_err());
    assert_eq!(
        login_with(&app, &restarted, "ok").await["status"],
        "succeeded"
    );
    let before = restarted.oauth.inference_cache().await.unwrap().unwrap();
    assert_eq!(
        login_with(&app, &restarted, "ok").await["status"],
        "succeeded"
    );
    let after = restarted.oauth.inference_cache().await.unwrap().unwrap();
    assert!(!Arc::ptr_eq(&before, &after));

    // Another instance has independent credentials and its own binding.
    let second_dir = tempfile::tempdir().unwrap();
    let second = state(second_dir.path());
    assert_eq!(
        login_with(&router(second.clone()), &second, "other").await["status"],
        "succeeded"
    );
    assert_eq!(
        second.oauth.status().unwrap()["binding"]["account_id"],
        "other-id"
    );
    assert_eq!(
        restarted.oauth.status().unwrap()["binding"]["account_id"],
        "fixture-id"
    );
    assert_ne!(
        second
            .oauth
            .inference_cache()
            .await
            .unwrap()
            .unwrap()
            .path(),
        after.path()
    );
}

#[test]
fn binding_and_credentials_commit_atomically_and_refresh_cannot_switch_identity() {
    let d = tempfile::tempdir().unwrap();
    let store = CredentialStore::open(&d.path().join("creds.redb")).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ["account-a", "account-b"]
        .into_iter()
        .map(|id| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut c = credential();
                c.native_config["oauthAccount"]["accountUuid"] = json!(id);
                barrier.wait();
                store.save(&c).is_ok()
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|h| h.join().unwrap().then_some(()))
            .count(),
        1
    );
    let mut saved = store.load().unwrap().unwrap();
    assert_eq!(
        store.status().unwrap()["binding"]["account_id"],
        saved.account_id().unwrap()
    );
    // Email changes and credential rotation are allowed for the same native ID.
    saved.email = Some("renamed@example.com".into());
    saved.access_token = "rotated-for-same-account".into();
    store.save(&saved).unwrap();
    let stable_id = saved.account_id().unwrap().to_owned();
    saved.native_config["oauthAccount"]["accountUuid"] = json!("other");
    assert_eq!(store.save(&saved).unwrap_err().status, StatusCode::CONFLICT);
    saved.native_config = Value::Null;
    assert_eq!(store.save(&saved).unwrap_err().status, StatusCode::CONFLICT);
    assert_eq!(
        store.load().unwrap().unwrap().access_token,
        "rotated-for-same-account"
    );
    assert_eq!(store.status().unwrap()["binding"]["account_id"], stable_id);
}

#[test]
fn old_databases_are_bound_before_logout_and_legacy_email_can_upgrade_once() {
    for has_native_id in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("legacy.redb");
        let mut c = credential();
        if !has_native_id {
            c.native_config = Value::Null;
        }
        // Seed an actual pre-binding database, bypassing the new save path.
        let db = redb::Database::create(&path).unwrap();
        let write = db.begin_write().unwrap();
        {
            let mut table = write
                .open_table(redb::TableDefinition::<&str, &[u8]>::new(
                    "oauth_credentials_v1",
                ))
                .unwrap();
            table
                .insert("active", serde_json::to_vec(&c).unwrap().as_slice())
                .unwrap();
        }
        write.commit().unwrap();
        drop(db);
        let store = CredentialStore::open(&path).unwrap();
        assert_eq!(
            store.status().unwrap()["binding"]["email"],
            "saved@example.com"
        );
        store.clear().unwrap();
        drop(store);
        let store = CredentialStore::open(&path).unwrap();
        let mut other = credential();
        other.email = Some("other@example.com".into());
        other.native_config["oauthAccount"]["accountUuid"] = json!("other");
        assert_eq!(store.save(&other).unwrap_err().status, StatusCode::CONFLICT);
        store.save(&credential()).unwrap();
        assert_eq!(
            store.status().unwrap()["binding"]["account_id"],
            "fixture-id"
        );
        // After migration even matching email cannot change the pinned UUID.
        other.email = Some("saved@example.com".into());
        assert_eq!(store.save(&other).unwrap_err().status, StatusCode::CONFLICT);
    }
}
