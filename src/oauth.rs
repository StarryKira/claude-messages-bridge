use crate::{
    error::{ApiError, Result},
    store::{Credential, CredentialStore},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

// Public-client endpoints/parameters observed in Claude Code 2.1.272.
// PKCE verifier stays in server memory; only the authorization URL reaches the browser.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
#[derive(Clone)]
pub struct Endpoints {
    pub token: String,
    pub profile: String,
    pub api_key: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            token: "https://platform.claude.com/v1/oauth/token".into(),
            profile: "https://api.anthropic.com/api/oauth/profile".into(),
            api_key: "https://api.anthropic.com/api/oauth/claude_cli/create_api_key".into(),
        }
    }
}
pub struct OAuthClient {
    client: reqwest::Client,
    pub store: Option<CredentialStore>,
    endpoints: Endpoints,
    refresh: Mutex<()>,
}
pub struct Authorization {
    pub url: String,
    pub state: String,
    pub verifier: String,
}
fn random_secret() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}
impl Authorization {
    pub fn new(method: &str, email: Option<&str>, sso: bool) -> Self {
        let verifier = random_secret();
        let state = random_secret();
        let mut url = Url::parse(if method == "console" {
            "https://platform.claude.com/oauth/authorize"
        } else {
            "https://claude.com/cai/oauth/authorize"
        })
        .unwrap();
        url.query_pairs_mut().extend_pairs([
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            (
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
            ("code_challenge_method", "S256"),
            ("state", &state),
        ]);
        if let Some(email) = email.filter(|v| !v.is_empty()) {
            url.query_pairs_mut().append_pair("login_hint", email);
        }
        if sso {
            url.query_pairs_mut().append_pair("login_method", "sso");
        }
        Self {
            url: url.into(),
            state,
            verifier,
        }
    }
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
    #[serde(default)]
    account: Value,
    #[serde(default)]
    organization: Value,
}
impl OAuthClient {
    pub fn new(store: Option<CredentialStore>, endpoints: Endpoints) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ApiError::upstream("Cannot initialize OAuth HTTP client"))?;
        Ok(Self {
            client,
            store,
            endpoints,
            refresh: Mutex::new(()),
        })
    }
    pub fn credentials(&self) -> Result<&CredentialStore> {
        self.store.as_ref().ok_or_else(|| {
            ApiError::upstream("Configure BRIDGE_CREDENTIAL_DB to enable OAuth credentials")
        })
    }
    async fn json(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let mut response = request.send().await.map_err(|_| {
            ApiError::upstream("OAuth request failed; check server network connectivity")
        })?;
        if !response.status().is_success() {
            // Provider bodies can contain credentials/codes. Return only status.
            return Err(ApiError::upstream(format!(
                "OAuth provider returned HTTP {}",
                response.status().as_u16()
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ApiError::upstream("OAuth response read failed"))?
        {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err(ApiError::upstream("OAuth response exceeded limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::upstream("Invalid OAuth response JSON"))
    }
    async fn token(&self, payload: Value) -> Result<TokenResponse> {
        let value = self
            .json(self.client.post(&self.endpoints.token).json(&payload))
            .await?;
        let tokens: TokenResponse = serde_json::from_value(value)
            .map_err(|_| ApiError::upstream("Invalid OAuth token response"))?;
        if tokens.access_token.is_empty()
            || tokens.access_token.len() > 32768
            || tokens.access_token.chars().any(char::is_control)
            || tokens.expires_in == 0
            || tokens.expires_in > 315360000
        {
            return Err(ApiError::upstream("Invalid OAuth token values"));
        }
        Ok(tokens)
    }
    pub async fn exchange(
        &self,
        auth: &Authorization,
        code: &str,
        method: &str,
    ) -> Result<Credential> {
        let tokens = self.token(json!({"grant_type":"authorization_code","code":code,"state":auth.state,"code_verifier":auth.verifier,"client_id":CLIENT_ID,"redirect_uri":REDIRECT_URI})).await?;
        let scopes: Vec<String> = tokens
            .scope
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let profile = self
            .json(
                self.client
                    .get(&self.endpoints.profile)
                    .bearer_auth(&tokens.access_token),
            )
            .await
            .ok();
        let field = |parent: &str, key: &str| {
            profile
                .as_ref()
                .and_then(|p| p[parent][key].as_str())
                .map(str::to_owned)
        };
        let api_key = if scopes.iter().any(|s| s == "user:inference") {
            None
        } else if method == "console" && scopes.iter().any(|s| s == "org:create_api_key") {
            let value = self
                .json(
                    self.client
                        .post(&self.endpoints.api_key)
                        .bearer_auth(&tokens.access_token)
                        .json(&Value::Null),
                )
                .await?;
            Some(
                value["raw_key"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| ApiError::upstream("Console OAuth did not return an API key"))?
                    .to_owned(),
            )
        } else {
            return Err(ApiError::upstream(
                "OAuth response is missing inference permission",
            ));
        };
        Ok(Credential {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: chrono::Utc::now().timestamp() + tokens.expires_in as i64,
            scopes,
            api_key,
            method: method.to_owned(),
            email: field("account", "email")
                .or_else(|| field("account", "email_address"))
                .or_else(|| tokens.account["email_address"].as_str().map(str::to_owned)),
            organization: field("organization", "name")
                .or_else(|| tokens.organization["name"].as_str().map(str::to_owned)),
            subscription: field("organization", "organization_type"),
        })
    }
    pub fn status(&self) -> Result<Value> {
        Ok(self.credentials()?.load()?.map(|c| c.public_view()).unwrap_or_else(|| json!({"logged_in":false,"method":null,"provider":"anthropic","email":null,"organization":null,"subscription":null,"storage":"redb"})))
    }
    pub async fn inference_credential(&self) -> Result<Option<Credential>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        // A single refresh per process; read again after acquiring the lock so rotated
        // refresh tokens cannot be overwritten by a concurrent stale response.
        let _lock = self.refresh.lock().await;
        let mut credential = store.load()?.ok_or_else(|| {
            ApiError::upstream("No service account connected; sign in through /admin/")
        })?;
        if credential.api_key.is_none()
            && credential.expires_at <= chrono::Utc::now().timestamp() + 300
        {
            let refresh = credential
                .refresh_token
                .as_ref()
                .ok_or_else(|| ApiError::upstream("OAuth session expired; sign in again"))?;
            let tokens = self.token(json!({"grant_type":"refresh_token","refresh_token":refresh,"client_id":CLIENT_ID,"scope":credential.scopes.join(" ")})).await?;
            credential.access_token = tokens.access_token;
            if tokens.refresh_token.is_some() {
                credential.refresh_token = tokens.refresh_token;
            }
            if let Some(scope) = tokens.scope {
                credential.scopes = scope.split_whitespace().map(str::to_owned).collect();
            }
            credential.expires_at = chrono::Utc::now().timestamp() + tokens.expires_in as i64;
            store.save(&credential)?;
        }
        Ok(Some(credential))
    }
}
