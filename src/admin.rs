use crate::{
    AppState,
    error::{ApiError, Result},
    same_secret,
};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path as FsPath;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;
use uuid::Uuid;

#[derive(Default)]
pub struct AdminState {
    operation: Mutex<Option<Operation>>,
}
struct Operation {
    view: LoginView,
    state: String,
    input: mpsc::Sender<String>,
    cancel: CancellationToken,
}
#[derive(Clone, Serialize)]
pub struct LoginView {
    id: String,
    status: String,
    authorization_url: Option<String>,
    message: Option<String>,
    expires_at: String,
}
impl LoginView {
    fn active(&self) -> bool {
        matches!(self.status.as_str(), "starting" | "waiting" | "submitting")
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    #[serde(default = "default_method")]
    method: String,
}
fn default_method() -> String {
    "claudeai".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeRequest {
    code: String,
}
pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/admin/status", get(status))
        .route("/api/admin/oauth", get(login_status))
        .route("/api/admin/oauth/start", post(start_login))
        .route("/api/admin/oauth/{id}", axum::routing::delete(cancel_login))
        .route("/api/admin/oauth/{id}/code", post(submit_code))
        .route("/api/admin/logout", post(logout))
        .layer(axum::extract::DefaultBodyLimit::max(8192))
        .route_layer(middleware::from_fn_with_state(state, authorize))
}
pub fn static_files(path: &FsPath) -> ServeDir {
    ServeDir::new(path)
}
pub async fn web_headers(request: Request, next: Next) -> Response {
    let is_web = request.uri().path().starts_with("/admin");
    let mut response = next.run(request).await;
    if is_web {
        for (key, val) in [
            (
                "content-security-policy",
                "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
            ),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ] {
            response
                .headers_mut()
                .insert(key, HeaderValue::from_static(val));
        }
    }
    response
}
async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    use axum::response::IntoResponse;
    let authorized = match &state.config.admin_token {
        None => Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            kind: "admin_disabled",
            message: "Set BRIDGE_ADMIN_TOKEN to enable the admin console".into(),
        }),
        Some(expected) => {
            let token = request
                .headers()
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "));
            if token.is_some_and(|t| same_secret(t.as_bytes(), expected.as_bytes())) {
                Ok(())
            } else {
                Err(ApiError {
                    status: StatusCode::UNAUTHORIZED,
                    kind: "authentication_error",
                    message: "Invalid admin token".into(),
                })
            }
        }
    };
    let mut response = match authorized {
        Ok(()) => next.run(request).await,
        Err(e) => e.into_response(),
    };
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}
fn conflict(message: &str) -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        kind: "conflict_error",
        message: message.into(),
    }
}
fn not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        kind: "not_found_error",
        message: "OAuth session not found".into(),
    }
}
async fn status(State(state): State<AppState>) -> Result<Json<Value>> {
    let account = state.oauth.status()?;
    let login = state
        .admin
        .operation
        .lock()
        .await
        .as_ref()
        .map(|op| op.view.clone());
    Ok(Json(json!({"account":account,"login":login,"service":{
        "version":env!("CARGO_PKG_VERSION"),"bind":state.config.bind.to_string(),
        "max_concurrency":state.config.concurrency,"available_slots":state.permits.available_permits(),
        "api_key_required":state.config.api_key.is_some(),"request_timeout_seconds":state.config.request_timeout.as_secs(),
        "credential_mode":"redb_oauth","messages_path":"/v1/messages"
    }})))
}
async fn login_status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"login":state.admin.operation.lock().await.as_ref().map(|op| op.view.clone())}))
}
async fn start_login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<(StatusCode, Json<LoginView>)> {
    state.oauth.credentials()?;
    if !matches!(request.method.as_str(), "claudeai" | "console") {
        return Err(ApiError::invalid("method must be claudeai or console"));
    }
    if state.shutdown.is_cancelled() {
        return Err(conflict("Server is shutting down"));
    }
    let mut operation = state.admin.operation.lock().await;
    if operation.as_ref().is_some_and(|op| op.view.active()) {
        return Err(conflict("An OAuth login is already running"));
    }
    let permit = state
        .permits
        .clone()
        .try_acquire_many_owned(
            state
                .config
                .concurrency
                .try_into()
                .map_err(|_| conflict("Invalid concurrency configuration"))?,
        )
        .map_err(|_| {
            conflict("Wait for active Messages requests or account operations to finish")
        })?;
    let mut auth = tokio::time::timeout(
        state.config.init_timeout,
        state.oauth.authorize(&state.config, &request.method),
    )
    .await
    .map_err(|_| ApiError::upstream("CLI authorization RPC timed out"))??;
    let id = Uuid::new_v4().to_string();
    let expires = chrono::Utc::now()
        + chrono::Duration::from_std(state.config.oauth_timeout)
            .map_err(|_| ApiError::invalid("OAuth timeout too large"))?;
    let view = LoginView {
        id: id.clone(),
        status: "waiting".into(),
        authorization_url: Some(auth.url.clone()),
        message: None,
        expires_at: expires.to_rfc3339(),
    };
    let cancel = state.shutdown.child_token();
    let (tx, mut rx) = mpsc::channel::<String>(1);
    *operation = Some(Operation {
        view: view.clone(),
        state: auth.state.clone(),
        input: tx,
        cancel: cancel.clone(),
    });
    let admin = state.admin.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let work = async {
            let code = rx.recv().await.ok_or_else(|| conflict("Login cancelled"))?;
            let credential = state.oauth.complete(&mut auth, &code).await?;
            // Serialize cancellation with the redb commit so a cancelled response
            // cannot race successfully committed credentials for the bound account.
            let mut operation = admin.operation.lock().await;
            if cancel.is_cancelled() {
                return Err(conflict("Login cancelled"));
            }
            state.oauth.commit(&auth, &credential).await?;
            if let Some(op) = operation.as_mut().filter(|op| op.view.id == id) {
                op.view.status = "succeeded".into();
                op.view.authorization_url = None;
            }
            Ok::<(), ApiError>(())
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => ("cancelled", Some("Login cancelled".into())),
            result = tokio::time::timeout(state.config.oauth_timeout, work) => match result {
                Err(_) => ("expired", Some("Login expired. Start a new authorization.".into())),
                Ok(Err(e)) => ("failed", Some(e.message)),
                Ok(Ok(())) => ("succeeded", None),
            }
        };
        auth.client.close().await;
        let mut op = admin.operation.lock().await;
        if let Some(op) = op.as_mut().filter(|op| op.view.id == id) {
            op.view.status = result.0.into();
            op.view.message = result.1;
            op.view.authorization_url = None;
        }
    });
    Ok((StatusCode::ACCEPTED, Json(view)))
}
async fn submit_code(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<CodeRequest>,
) -> Result<Json<Value>> {
    let code = request.code.trim();
    if code.len() > 4096
        || code.chars().any(char::is_whitespace)
        || code.chars().any(char::is_control)
    {
        return Err(ApiError::invalid("Invalid authorization code"));
    }
    let (value, supplied_state) = code
        .split_once('#')
        .filter(|(v, s)| !v.is_empty() && !s.is_empty() && !s.contains('#'))
        .ok_or_else(|| ApiError::invalid("Paste the complete code#state value"))?;
    let mut op = state.admin.operation.lock().await;
    let op = op
        .as_mut()
        .filter(|op| op.view.id == id)
        .ok_or_else(not_found)?;
    if op.view.status != "waiting" {
        return Err(conflict("OAuth session is not waiting for a code"));
    }
    if !same_secret(op.state.as_bytes(), supplied_state.as_bytes()) {
        return Err(ApiError::invalid(
            "Authorization state does not match this login session",
        ));
    }
    op.input
        .try_send(value.to_owned())
        .map_err(|_| conflict("OAuth session is not accepting a code"))?;
    op.view.status = "submitting".into();
    op.view.message = None;
    Ok(Json(json!({"accepted":true})))
}
async fn cancel_login(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let mut op = state.admin.operation.lock().await;
    let op = op
        .as_mut()
        .filter(|op| op.view.id == id)
        .ok_or_else(not_found)?;
    if op.view.active() {
        op.cancel.cancel();
        op.view.status = "cancelled".into();
        op.view.authorization_url = None;
    }
    Ok(Json(json!({"cancelled":true})))
}
async fn logout(State(state): State<AppState>) -> Result<Json<Value>> {
    let op = state.admin.operation.lock().await;
    if op.as_ref().is_some_and(|op| op.view.active()) {
        return Err(conflict("Cancel the active OAuth login before logging out"));
    }
    let _permit = state
        .permits
        .clone()
        .try_acquire_many_owned(
            state
                .config
                .concurrency
                .try_into()
                .map_err(|_| conflict("Invalid concurrency configuration"))?,
        )
        .map_err(|_| conflict("Wait for active requests to finish before logging out"))?;
    state.oauth.logout().await?;
    Ok(Json(json!({"logged_out":true})))
}
