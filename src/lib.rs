pub mod admin;
pub mod config;
pub mod error;
pub mod history;
pub mod oauth;
pub mod request;
pub mod response;
pub mod rpc;
pub mod store;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use error::ApiError;
use futures_util::stream;
use request::MessagesRequest;
use response::MessageAccumulator;
use serde_json::json;
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<config::Config>,
    pub permits: Arc<Semaphore>,
    pub shutdown: CancellationToken,
    pub admin: Arc<admin::AdminState>,
    pub oauth: Arc<oauth::OAuthClient>,
}
impl AppState {
    pub fn new(config: config::Config) -> Self {
        Self::try_new(config).expect("Cannot initialize bridge state")
    }
    pub fn try_new(config: config::Config) -> error::Result<Self> {
        let store = config
            .credential_db_path
            .as_deref()
            .map(store::CredentialStore::open)
            .transpose()?;
        let oauth = Arc::new(oauth::OAuthClient::new(store, oauth::Endpoints::default())?);
        Ok(Self {
            permits: Arc::new(Semaphore::new(config.concurrency)),
            config: Arc::new(config),
            shutdown: CancellationToken::new(),
            admin: Arc::new(admin::AdminState::default()),
            oauth,
        })
    }
}
pub fn router(state: AppState) -> Router {
    let max_body = state.config.max_body_bytes;
    Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/messages", post(messages))
        .merge(admin::routes(state.clone()))
        .route("/", get(|| async { axum::response::Redirect::temporary("/admin/") }))
        .nest_service("/admin", admin::static_files(&state.config.web_dir))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"type":"error","error":{"type":"not_found_error","message":"Route not found"}}))) })
        .method_not_allowed_fallback(|| async { (StatusCode::METHOD_NOT_ALLOWED, Json(json!({"type":"error","error":{"type":"invalid_request_error","message":"Method not allowed"}}))) })
        .layer(DefaultBodyLimit::max(max_body))
        .layer(axum::middleware::from_fn(admin::web_headers))
        .with_state(state)
}
async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: std::result::Result<Json<MessagesRequest>, JsonRejection>,
) -> Response {
    let request_id = format!("req_{}", Uuid::new_v4().simple());
    let mut response = handle(state, headers, body)
        .await
        .unwrap_or_else(IntoResponse::into_response);
    response
        .headers_mut()
        .insert("request-id", HeaderValue::from_str(&request_id).unwrap());
    response
}
async fn handle(
    state: AppState,
    headers: HeaderMap,
    body: std::result::Result<Json<MessagesRequest>, JsonRejection>,
) -> error::Result<Response> {
    if let Some(expected) = &state.config.api_key {
        let supplied = headers
            .get("x-api-key")
            .and_then(|h| h.to_str().ok())
            .or_else(|| {
                headers
                    .get("authorization")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.strip_prefix("Bearer "))
            });
        if !supplied.is_some_and(|s| same_secret(s.as_bytes(), expected.as_bytes())) {
            return Err(ApiError {
                status: StatusCode::UNAUTHORIZED,
                kind: "authentication_error",
                message: "Invalid bridge API key".into(),
            });
        }
    }
    if headers
        .get("anthropic-version")
        .is_some_and(|h| h != "2023-06-01")
    {
        return Err(ApiError::invalid("Supported anthropic-version: 2023-06-01"));
    }
    if headers.contains_key("anthropic-beta") {
        return Err(ApiError::invalid(
            "anthropic-beta features are not supported by this CLI adapter",
        ));
    }
    let Json(request) = body.map_err(|e| ApiError {
        status: if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        },
        kind: if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            "request_too_large"
        } else {
            "invalid_request_error"
        },
        message: e.body_text(),
    })?;
    let request = request.validate()?;
    if state.shutdown.is_cancelled() {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            kind: "overloaded_error",
            message: "Server is shutting down".into(),
        });
    }
    let permit = state
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            kind: "rate_limit_error",
            message: "All CLI request slots are busy; retry later".into(),
        })?;
    let streaming = request.stream;
    let rpc::Session { mut events, cancel } =
        rpc::start(state.config, state.oauth, request, permit, state.shutdown);
    // Wait for the first event so spawn/auth/initialize failures retain an HTTP error status.
    let first = events
        .recv()
        .await
        .ok_or_else(|| ApiError::upstream("CLI request ended without output"))??;
    if streaming {
        let stream = stream::unfold(
            (Some(first), events, cancel, false),
            |(first, mut events, guard, done)| async move {
                if done {
                    return None;
                }
                let item = match first {
                    Some(e) => Some(Ok(e)),
                    None => events.recv().await,
                };
                let (value, done) = match item {
                    Some(Ok(e)) => {
                        let done = e["type"] == "message_stop";
                        (e, done)
                    }
                    Some(Err(e)) => (e.body(), true),
                    None => (
                        ApiError::upstream("CLI stream ended unexpectedly").body(),
                        true,
                    ),
                };
                let event = Event::default()
                    .event(value["type"].as_str().unwrap_or("error"))
                    .json_data(&value)
                    .expect("JSON Value serialization cannot fail");
                Some((Ok::<_, Infallible>(event), (None, events, guard, done)))
            },
        );
        let mut response = Sse::new(stream)
            .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
            .into_response();
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-cache"));
        response
            .headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
        Ok(response)
    } else {
        let _guard = cancel;
        let mut accumulator = MessageAccumulator::default();
        accumulator.push(&first)?;
        while !accumulator.complete() {
            let event = events
                .recv()
                .await
                .ok_or_else(|| ApiError::upstream("CLI stream ended unexpectedly"))??;
            accumulator.push(&event)?;
        }
        Ok(Json(accumulator.finish()?).into_response())
    }
}
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    let mut difference = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        difference |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;
    }
    difference == 0
}
