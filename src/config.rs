use std::{collections::BTreeMap, env, net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub cli: PathBuf,
    pub api_key: Option<String>,
    pub admin_token: Option<String>,
    pub credential_db_path: Option<PathBuf>,
    pub web_dir: PathBuf,
    pub oauth_timeout: Duration,
    pub concurrency: usize,
    pub request_timeout: Duration,
    pub init_timeout: Duration,
    pub max_body_bytes: usize,
    pub max_frame_bytes: usize,
    pub max_output_bytes: usize,
    // Server-owned overrides, never supplied by an HTTP caller. Useful for isolated testing.
    pub cli_env: BTreeMap<String, String>,
    pub bare: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8787".parse().unwrap(),
            cli: PathBuf::from("claude"),
            api_key: None,
            admin_token: None,
            credential_db_path: None,
            web_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/dist"),
            oauth_timeout: Duration::from_secs(600),
            concurrency: 4,
            request_timeout: Duration::from_secs(180),
            init_timeout: Duration::from_secs(30),
            max_body_bytes: 32 * 1024 * 1024,
            max_frame_bytes: 16 * 1024 * 1024,
            max_output_bytes: 32 * 1024 * 1024,
            cli_env: BTreeMap::new(),
            bare: false,
        }
    }
}
impl Config {
    pub fn from_env() -> std::result::Result<Self, String> {
        let mut c = Self::default();
        if let Ok(v) = env::var("BRIDGE_BIND") {
            c.bind = v.parse().map_err(|_| "Invalid BRIDGE_BIND")?;
        }
        if let Some(v) = env::var_os("CLAUDE_CLI_PATH") {
            c.cli = v.into();
        } else if let Some(home) = env::var_os("HOME") {
            let local = PathBuf::from(home).join(".local/bin/claude");
            if local.is_file() {
                c.cli = local;
            }
        }
        c.api_key = env::var("BRIDGE_API_KEY").ok().filter(|s| !s.is_empty());
        c.admin_token = env::var("BRIDGE_ADMIN_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        if let Some(token) = &c.admin_token {
            if token.len() < 32 {
                return Err("BRIDGE_ADMIN_TOKEN must contain at least 32 bytes".into());
            }
            if c.api_key.as_ref() == Some(token) {
                return Err("BRIDGE_ADMIN_TOKEN and BRIDGE_API_KEY must be different".into());
            }
        }
        c.credential_db_path = env::var_os("BRIDGE_CREDENTIAL_DB").map(PathBuf::from);
        if c.admin_token.is_some() && c.credential_db_path.is_none() {
            let home =
                env::var_os("HOME").ok_or("Set BRIDGE_CREDENTIAL_DB when HOME is unavailable")?;
            c.credential_db_path =
                Some(PathBuf::from(home).join(".claude-messages-bridge/credentials.redb"));
        }
        if let Some(path) = &c.credential_db_path {
            c.credential_db_path =
                Some(std::path::absolute(path).map_err(|_| "Invalid BRIDGE_CREDENTIAL_DB")?);
        }
        if let Some(dir) = env::var_os("BRIDGE_WEB_DIR") {
            c.web_dir = PathBuf::from(dir);
        }
        c.oauth_timeout = Duration::from_secs(number("BRIDGE_OAUTH_TIMEOUT_SECONDS", 600)? as u64);
        c.concurrency = number("BRIDGE_MAX_CONCURRENCY", c.concurrency)?;
        c.request_timeout = Duration::from_secs(number("BRIDGE_TIMEOUT_SECONDS", 180)? as u64);
        c.init_timeout = Duration::from_secs(number("BRIDGE_INIT_TIMEOUT_SECONDS", 30)? as u64);
        c.bare = env::var("BRIDGE_CLI_BARE").as_deref() == Ok("1");
        if c.bare && c.admin_token.is_some() {
            return Err("OAuth administration requires BRIDGE_CLI_BARE=0".into());
        }
        if !c.bind.ip().is_loopback() && c.api_key.is_none() {
            return Err("Set BRIDGE_API_KEY before binding to a non-loopback address".into());
        }
        Ok(c)
    }

    // In managed mode, redb is the only persistent credential source.
    pub fn apply_cli_env(&self, command: &mut tokio::process::Command) {
        command
            .envs(&self.cli_env)
            .env_remove("BRIDGE_ADMIN_TOKEN")
            .env_remove("BRIDGE_API_KEY");
        if self.credential_db_path.is_some() {
            for key in [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
                "CLAUDE_CODE_OAUTH_SCOPES",
                "CLAUDE_CODE_OAUTH_CLIENT_ID",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
                "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
                "CLAUDE_CODE_SESSION_ACCESS_TOKEN",
                "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
            ] {
                command.env_remove(key);
            }
        }
    }
}

fn number(name: &str, default: usize) -> std::result::Result<usize, String> {
    match env::var(name) {
        Ok(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(_) => Err(format!("Invalid {name}")),
    }
}
