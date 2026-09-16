//! Account lifecycle via native CLI RPC. The bridge never implements OAuth HTTP.
use crate::{
    config::Config,
    control::ControlClient,
    credential_cache::CredentialCache,
    error::{ApiError, Result},
    store::CredentialStore,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use url::Url;

pub struct OAuthClient {
    pub store: Option<CredentialStore>,
    cache: Mutex<Option<Arc<CredentialCache>>>,
}
pub struct Authorization {
    pub url: String,
    pub state: String,
    pub client: ControlClient,
    cache: Arc<CredentialCache>,
    method: String,
}
impl OAuthClient {
    pub fn new(store: Option<CredentialStore>) -> Self {
        Self {
            store,
            cache: Mutex::new(None),
        }
    }
    pub fn credentials(&self) -> Result<&CredentialStore> {
        self.store.as_ref().ok_or_else(|| {
            ApiError::upstream("Configure BRIDGE_CREDENTIAL_DB to enable account management")
        })
    }
    pub async fn authorize(&self, config: &Config, method: &str) -> Result<Authorization> {
        self.credentials()?;
        let cache = Arc::new(CredentialCache::new(None)?);
        let mut client = ControlClient::start(config, &cache).await?;
        let response = client
            .call(json!({"subtype":"claude_authenticate","loginWithClaudeAi":method=="claudeai"}))
            .await?;
        let url = response["manualUrl"]
            .as_str()
            .ok_or_else(|| ApiError::upstream("CLI did not return an OAuth authorization URL"))?
            .to_owned();
        let parsed = Url::parse(&url)
            .map_err(|_| ApiError::upstream("CLI returned an invalid authorization URL"))?;
        if parsed.scheme() != "https"
            || !matches!(
                parsed.host_str(),
                Some(
                    "claude.com"
                        | "claude.ai"
                        | "platform.claude.com"
                        | "console.anthropic.com"
                        | "beacon.claude-ai.staging.ant.dev"
                        | "claude.fedstart.com"
                        | "claude-staging.fedstart.com"
                )
            )
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(ApiError::upstream(
                "CLI returned an unexpected authorization URL",
            ));
        }
        let state = parsed
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ApiError::upstream("CLI authorization URL has no state"))?;
        Ok(Authorization {
            url,
            state,
            client,
            cache,
            method: method.into(),
        })
    }
    pub async fn complete(
        &self,
        auth: &mut Authorization,
        code: &str,
    ) -> Result<crate::store::Credential> {
        let response=auth.client.call(json!({"subtype":"claude_oauth_callback","authorizationCode":code,"state":auth.state})).await?;
        // This RPC itself awaits the CLI's flow, including token storage and policy
        // checks. Do not send wait_for_completion after the completed flow is gone.
        let credential = auth
            .cache
            .capture(None, &auth.method, response.get("account"))?;
        if credential.account_id().is_none() {
            return Err(ApiError::upstream(
                "CLI did not provide an account identity; credentials were not saved",
            ));
        }
        Ok(credential)
    }
    pub async fn commit(
        &self,
        auth: &Authorization,
        credential: &crate::store::Credential,
    ) -> Result<()> {
        let mut cache = self.cache.lock().await;
        self.credentials()?.save(credential)?;
        *cache = Some(auth.cache.clone());
        Ok(())
    }
    pub fn status(&self) -> Result<Value> {
        self.credentials()?.status()
    }
    pub async fn inference_cache(&self) -> Result<Option<Arc<CredentialCache>>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let mut cache = self.cache.lock().await;
        if cache.is_none() {
            let credential = store.load()?.ok_or_else(|| {
                ApiError::upstream("No service account connected; sign in through /admin/")
            })?;
            *cache = Some(Arc::new(CredentialCache::new(Some(&credential))?));
        }
        Ok(cache.clone())
    }
    pub async fn sync(&self, used: &Arc<CredentialCache>) -> Result<()> {
        let active = self.cache.lock().await;
        if !active.as_ref().is_some_and(|c| Arc::ptr_eq(c, used)) {
            return Ok(());
        }
        let store = self.credentials()?;
        if let Some(previous) = store.load()? {
            let credential = used.capture(Some(&previous), &previous.method, None)?;
            store.save(&credential)?;
        }
        Ok(())
    }
    pub async fn logout(&self) -> Result<()> {
        let mut cache = self.cache.lock().await;
        self.credentials()?.clear()?;
        *cache = None;
        Ok(())
    }
}
