//! CLI-native working files. redb is the durable source; this private cache is
//! shared by sibling CLI processes so the CLI's native refresh lock/CAS works.
use crate::{
    error::{ApiError, Result},
    store::Credential,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use tokio::process::Command;

pub struct CredentialCache {
    directory: tempfile::TempDir,
}
fn cache_error(_: impl std::fmt::Display) -> ApiError {
    ApiError::upstream("CLI credential cache operation failed")
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(cache_error)?
        .write_all(bytes)
        .map_err(cache_error)
}
fn read_optional(path: PathBuf) -> Result<Option<String>> {
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(cache_error(e)),
        Ok(m) if !m.is_file() || m.len() > 256 * 1024 => return Err(cache_error("Invalid file")),
        _ => {}
    }
    std::fs::read_to_string(path).map(Some).map_err(cache_error)
}
impl CredentialCache {
    pub fn new(credential: Option<&Credential>) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("claude-bridge-auth-")
            .tempdir()
            .map_err(cache_error)?;
        let cache = Self { directory };
        let mut config = credential
            .map(|c| c.native_config.clone())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        if let Some(c) = credential {
            let native = if c.native_credentials.is_object() {
                c.native_credentials.clone()
            } else if c.api_key.is_some() {
                json!({})
            } else {
                json!({"claudeAiOauth":{"accessToken":c.access_token,"refreshToken":c.refresh_token,"expiresAt":c.expires_at.saturating_mul(1000),"scopes":c.scopes,"subscriptionType":c.subscription}})
            };
            write_private(
                &cache.path().join(".credentials.json"),
                &serde_json::to_vec(&native).map_err(cache_error)?,
            )?;
            if let Some(key) = &c.api_key {
                write_private(&cache.path().join(".console-key"), key.as_bytes())?;
                config["primaryApiKey"] = json!(key);
            }
        }
        write_private(
            &cache.path().join(".claude.json"),
            &serde_json::to_vec(&config).map_err(cache_error)?,
        )?;
        // The Darwin CLI calls `security`; adapt only this child's credential
        // namespace to working files, without touching the user's OS keychain.
        if cfg!(target_os = "macos") {
            let bin = cache.path().join("bin");
            std::fs::create_dir(&bin).map_err(cache_error)?;
            let helper = bin.join("security");
            write_private(&helper, include_bytes!("../scripts/credential-store.py"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(helper, std::fs::Permissions::from_mode(0o700))
                    .map_err(cache_error)?;
            }
        }
        Ok(cache)
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    pub fn apply(&self, command: &mut Command) -> Result<()> {
        command
            .env("CLAUDE_CONFIG_DIR", self.path())
            .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", self.path());
        if cfg!(target_os = "macos") {
            let inherited = command
                .as_std()
                .get_envs()
                .find(|(k, _)| *k == "PATH")
                .and_then(|(_, v)| v.map(ToOwned::to_owned))
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default();
            let mut paths = vec![self.path().join("bin")];
            paths.extend(std::env::split_paths(&inherited));
            command.env("PATH", std::env::join_paths(paths).map_err(cache_error)?);
        }
        Ok(())
    }
    pub fn capture(
        &self,
        baseline: Option<&Credential>,
        method: &str,
        account: Option<&Value>,
    ) -> Result<Credential> {
        let raw = read_optional(self.path().join(".keychain-credentials.json"))?
            .or(read_optional(self.path().join(".credentials.json"))?)
            .map(|s| serde_json::from_str::<Value>(&s).map_err(cache_error))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        let config = read_optional(self.path().join(".claude.json"))?
            .map(|s| serde_json::from_str::<Value>(&s).map_err(cache_error))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        let tokens = &raw["claudeAiOauth"];
        let api_key = read_optional(self.path().join(".console-key"))?
            .or_else(|| config["primaryApiKey"].as_str().map(str::to_owned));
        let access = tokens["accessToken"].as_str().unwrap_or("");
        if access.is_empty() && api_key.as_deref().is_none_or(str::is_empty) {
            return Err(ApiError::upstream(
                "CLI completed authentication without saved credentials",
            ));
        }
        let metadata = account.unwrap_or(&Value::Null);
        let field = |key: &str| metadata[key].as_str().map(str::to_owned);
        let mut saved_config = json!({});
        for key in [
            "oauthAccount",
            "userID",
            "hasCompletedOnboarding",
            "customApiKeyResponses",
        ] {
            if let Some(value) = config.get(key) {
                saved_config[key] = value.clone();
            }
        }
        Ok(Credential {
            access_token: access.into(),
            refresh_token: tokens["refreshToken"].as_str().map(str::to_owned),
            expires_at: tokens["expiresAt"].as_i64().unwrap_or(0) / 1000,
            scopes: tokens["scopes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            api_key,
            method: method.into(),
            email: field("email")
                .or_else(|| {
                    config["oauthAccount"]["emailAddress"]
                        .as_str()
                        .map(str::to_owned)
                })
                .or_else(|| baseline.and_then(|c| c.email.clone())),
            organization: field("organization")
                .or_else(|| baseline.and_then(|c| c.organization.clone())),
            subscription: field("subscriptionType")
                .or_else(|| tokens["subscriptionType"].as_str().map(str::to_owned))
                .or_else(|| baseline.and_then(|c| c.subscription.clone())),
            native_credentials: if tokens.is_object() {
                json!({"claudeAiOauth":tokens})
            } else {
                json!({})
            },
            native_config: saved_config,
        })
    }
}
