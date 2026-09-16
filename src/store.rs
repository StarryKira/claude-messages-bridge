use crate::error::{ApiError, Result};
use redb::{Database, TableDefinition};
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
    pub fn public_view(&self) -> Value {
        json!({"logged_in":true,"method":self.method,"provider":"anthropic",
            "email":self.email,"organization":self.organization,"subscription":self.subscription,
            "expires_at":self.expires_at,"storage":"redb"})
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
        write.open_table(CREDENTIALS).map_err(database_error)?;
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
            table
                .insert("active", bytes.as_slice())
                .map_err(database_error)?;
        }
        write.commit().map_err(database_error)
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
