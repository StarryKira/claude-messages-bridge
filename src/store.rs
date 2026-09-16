use crate::error::{ApiError, Result};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const CREDENTIALS: TableDefinition<&str, &[u8]> = TableDefinition::new("oauth_credentials_v1");
// Do not derive Debug: token values must never appear in diagnostics.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,
    pub scopes: Vec<String>,
    pub api_key: Option<String>,
    pub method: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub subscription: Option<String>,
    #[serde(default)]
    pub native_credentials: Value,
    #[serde(default)]
    pub native_config: Value,
}
impl Credential {
    pub fn account_id(&self) -> Option<&str> {
        self.native_config["oauthAccount"]["accountUuid"]
            .as_str()
            .filter(|id| !id.is_empty())
    }
    pub fn public_view(&self) -> Value {
        json!({"logged_in":true,"method":self.method,"provider":"anthropic",
            "email":self.email,"organization":self.organization,"subscription":self.subscription,
            "expires_at":self.expires_at,"storage":"redb"})
    }
}
// Kept after logout: one database represents one account for the instance's lifetime.
#[derive(Serialize, Deserialize)]
struct AccountBinding {
    account_id: Option<String>,
    email: Option<String>,
}
impl AccountBinding {
    fn from_credential(credential: &Credential) -> Self {
        Self {
            account_id: credential.account_id().map(str::to_owned),
            email: credential.email.clone().filter(|email| !email.is_empty()),
        }
    }
    fn accepts(&self, credential: &Credential) -> bool {
        if let Some(id) = &self.account_id {
            credential.account_id() == Some(id.as_str())
        } else {
            // Only pre-binding databases may use email to upgrade to a native ID.
            // Missing identity never makes an existing instance unbound.
            self.email
                .as_ref()
                .is_some_and(|email| credential.email.as_ref() == Some(email))
        }
    }
}
#[derive(Clone)]
pub struct CredentialStore {
    db: Arc<Database>,
}
fn database_error(_: impl std::fmt::Display) -> ApiError {
    ApiError::upstream("Credential database operation failed")
}
impl CredentialStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(database_error)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(database_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(database_error)?;
        }
        let db = Database::builder()
            .create_file(file)
            .map_err(database_error)?;
        let write = db.begin_write().map_err(database_error)?;
        {
            let mut table = write.open_table(CREDENTIALS).map_err(database_error)?;
            let bound = table.get("binding").map_err(database_error)?.is_some();
            if !bound {
                let existing: Option<Credential> = table
                    .get("active")
                    .map_err(database_error)?
                    .map(|bytes| serde_json::from_slice(bytes.value()).map_err(database_error))
                    .transpose()?;
                if let Some(credential) = existing {
                    let bytes = serde_json::to_vec(&AccountBinding::from_credential(&credential))
                        .map_err(database_error)?;
                    table
                        .insert("binding", bytes.as_slice())
                        .map_err(database_error)?;
                }
            }
        }
        write.commit().map_err(database_error)?;
        Ok(Self { db: Arc::new(db) })
    }
    pub fn load(&self) -> Result<Option<Credential>> {
        let read = self.db.begin_read().map_err(database_error)?;
        let table = read.open_table(CREDENTIALS).map_err(database_error)?;
        table
            .get("active")
            .map_err(database_error)?
            .map(|bytes| serde_json::from_slice(bytes.value()).map_err(database_error))
            .transpose()
    }
    pub fn save(&self, credential: &Credential) -> Result<()> {
        let bytes = serde_json::to_vec(credential).map_err(database_error)?;
        let write = self.db.begin_write().map_err(database_error)?;
        {
            let mut table = write.open_table(CREDENTIALS).map_err(database_error)?;
            let binding: Option<AccountBinding> = table
                .get("binding")
                .map_err(database_error)?
                .map(|bytes| serde_json::from_slice(bytes.value()).map_err(database_error))
                .transpose()?;
            if let Some(binding) = binding {
                if !binding.accepts(credential) {
                    return Err(ApiError {
                        status: axum::http::StatusCode::CONFLICT,
                        kind: "invalid_request_error",
                        message: "This instance is bound to another account. Sign in with the bound account or run a separate instance with its own credential database.".into(),
                    });
                }
            } else if credential.account_id().is_none() {
                return Err(ApiError::upstream(
                    "CLI did not provide an account identity; this instance was not bound",
                ));
            }
            let binding = serde_json::to_vec(&AccountBinding::from_credential(credential))
                .map_err(database_error)?;
            table
                .insert("binding", binding.as_slice())
                .map_err(database_error)?;
            table
                .insert("active", bytes.as_slice())
                .map_err(database_error)?;
        }
        write.commit().map_err(database_error)
    }
    pub fn status(&self) -> Result<Value> {
        let read = self.db.begin_read().map_err(database_error)?;
        let table = read.open_table(CREDENTIALS).map_err(database_error)?;
        let credential: Option<Credential> = table
            .get("active")
            .map_err(database_error)?
            .map(|bytes| serde_json::from_slice(bytes.value()).map_err(database_error))
            .transpose()?;
        let binding: Option<AccountBinding> = table
            .get("binding")
            .map_err(database_error)?
            .map(|bytes| serde_json::from_slice(bytes.value()).map_err(database_error))
            .transpose()?;
        let mut status = credential.map(|c| c.public_view()).unwrap_or_else(|| {
            json!({
                "logged_in":false,"method":null,"provider":"anthropic","email":null,
                "organization":null,"subscription":null,"storage":"redb"
            })
        });
        status["binding"] = serde_json::to_value(binding).map_err(database_error)?;
        Ok(status)
    }
    pub fn clear(&self) -> Result<()> {
        let write = self.db.begin_write().map_err(database_error)?;
        {
            let mut table = write.open_table(CREDENTIALS).map_err(database_error)?;
            table.remove("active").map_err(database_error)?;
        }
        write.commit().map_err(database_error)
    }
}
