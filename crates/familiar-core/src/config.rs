use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Any Postgres (session mode: LISTEN needs it). Unset in the desktop app = its built-in database.
    #[serde(default)]
    pub database_url: Option<String>,
    /// The owner's user id. Unset = the single account in the database (waits until it's created).
    #[serde(default)]
    pub owner_id: Option<Uuid>,
    /// Desktop app: password of the built-in Postgres (generated on first launch).
    #[serde(default)]
    pub embedded_db_password: Option<String>,
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
    #[serde(default = "default_claude_bin")]
    pub claude_bin: String,
    /// OpenAI Codex CLI for bots on the `codex` engine. On Windows the npm `.cmd` shim is resolved from PATH.
    #[serde(default = "default_codex_bin")]
    pub codex_bin: String,
    #[serde(default)]
    pub bots_dir: Option<PathBuf>,
    #[serde(default)]
    pub device_name: Option<String>,
    /// familiar-server base URL; pinged so a free instance that sleeps when idle stays awake while this PC is on.
    #[serde(default)]
    pub server_url: Option<String>,
    /// Chrome/Edge/Chromium for the bots' browsers (default: auto-detect).
    #[serde(default)]
    pub browser_bin: Option<String>,
    /// Desktop app only: also serve the Familiar API on 127.0.0.1:<port> (no separate server process needed locally).
    #[serde(default)]
    pub local_api_port: Option<u16>,
    /// Same base64 key as the server's FAMILIAR_SECRET_KEY; decrypts connector and channel secrets.
    #[serde(default)]
    pub secret_key: Option<String>,
    /// S3-compatible artifact store (Cloudflare R2, MinIO, AWS). Without it, files ≤ 5 MB go to Postgres.
    #[serde(default)]
    pub s3: Option<S3>,
    /// How long a spawned `claude` may stay silent before the run fails as stuck (default 150 s; tests lower it).
    #[serde(default)]
    pub startup_timeout_secs: Option<u64>,
    /// Look up which models this computer's Claude plan and Codex CLI offer (`models.rs`; default on, tests turn it
    /// off so the fake CLIs see only run invocations).
    #[serde(default = "default_true")]
    pub check_models: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct S3 {
    pub endpoint: String,
    pub bucket: String,
    #[serde(default = "default_region")]
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

fn default_region() -> String {
    "auto".into()
}

fn default_max_parallel() -> usize {
    2
}

fn default_claude_bin() -> String {
    "claude".into()
}

fn default_codex_bin() -> String {
    "codex".into()
}

impl Config {
    /// `~/.familiar`, or `FAMILIAR_HOME` when set (tests, portable installs).
    pub fn home_dir() -> PathBuf {
        if let Some(home) = std::env::var_os("FAMILIAR_HOME").filter(|h| !h.is_empty()) {
            return PathBuf::from(home);
        }
        let Some(base) = directories::BaseDirs::new() else {
            return PathBuf::from(".familiar");
        };
        let new = base.home_dir().join(".familiar");
        let old = base.home_dir().join(".zed");
        // Upgrade from the product's former name: move the old home over once; if that fails keep using it.
        if !new.exists() && old.exists() && let Err(e) = std::fs::rename(&old, &new) {
            tracing::warn!("could not rename {} to {}: {e}; continuing with the old folder", old.display(), new.display());
            return old;
        }
        new
    }

    /// An env var by its `FAMILIAR_*` name, falling back to the pre-rename `ZED_*` name.
    pub fn env_var(suffix: &str) -> Option<String> {
        std::env::var(format!("FAMILIAR_{suffix}"))
            .or_else(|_| std::env::var(format!("ZED_{suffix}")))
            .ok()
    }

    /// `~/.familiar/config.toml`, then `FAMILIAR_*` env vars on top.
    pub fn load() -> Result<Self> {
        let path = Self::home_dir().join("config.toml");
        let mut table: toml::Table = match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
            Err(_) => toml::Table::new(),
        };
        for key in ["database_url", "owner_id", "max_parallel", "claude_bin", "codex_bin", "bots_dir", "device_name", "server_url", "secret_key", "startup_timeout_secs"] {
            if let Some(v) = Self::env_var(&key.to_uppercase()) {
                let value = match v.parse::<i64>() {
                    Ok(n) if matches!(key, "max_parallel" | "startup_timeout_secs") => toml::Value::Integer(n),
                    _ => toml::Value::String(v),
                };
                table.insert(key.into(), value);
            }
        }
        table.try_into().with_context(|| format!("invalid config in {}", path.display()))
    }

    pub fn path() -> PathBuf {
        Self::home_dir().join("config.toml")
    }

    /// First launch of the desktop app: a config with fresh secrets and the built-in database. Never overwrites.
    pub fn create_default() -> Result<()> {
        let path = Self::path();
        if path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(Self::home_dir())?;
        let password = uuid::Uuid::new_v4().simple().to_string();
        let text = format!(
            "# Familiar config. Created on first launch; keep it private (it holds secrets).\n\
             # Leave database_url unset to use the built-in database, or point it at any Postgres.\n\
             secret_key = \"{}\"\nembedded_db_password = \"{password}\"\nlocal_api_port = 47080\nmax_parallel = 2\n",
            familiar_crypto::SecretBox::generate_key(),
        );
        // Secrets inside: readable by this user only (Windows profiles are private already).
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            opts.mode(0o600);
            let _ = std::fs::set_permissions(Self::home_dir(), std::fs::Permissions::from_mode(0o700));
        }
        use std::io::Write;
        opts.open(&path)?.write_all(text.as_bytes())?;
        Ok(())
    }

    /// Silence allowed from a freshly spawned `claude` before the run fails as stuck. MCP startup may take up to
    /// MCP_TIMEOUT (120 s), so the default leaves headroom beyond that.
    pub fn startup_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.startup_timeout_secs.unwrap_or(150))
    }

    pub fn bots_dir(&self) -> PathBuf {
        self.bots_dir.clone().unwrap_or_else(|| Self::home_dir().join("bots"))
    }

    /// Stable per-machine id, persisted next to the config.
    pub fn device_id() -> Uuid {
        let path = Self::home_dir().join("device_id");
        if let Some(id) = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse().ok()) {
            return id;
        }
        let id = Uuid::new_v4();
        let _ = std::fs::create_dir_all(Self::home_dir());
        let _ = std::fs::write(&path, id.to_string());
        id
    }
}
