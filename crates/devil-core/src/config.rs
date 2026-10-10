//! Global configuration: `~/.config/devil/config.toml` (or `$DEVIL_HOME`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::{devil_home, DEFAULT_REGISTRY};
use crate::error::Error;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Config {
    /// Base URL of the registry used when a dependency does not name one.
    pub registry: String,
    /// Write downloads and diagnostics in colour.
    pub color: bool,
    /// Never prompt; fail instead.
    pub offline: bool,
    /// Concurrency for downloads.
    pub jobs: usize,
    /// Verbose build output.
    pub verbose: bool,
    /// API token, normally left unset and read from `DEVIL_TOKEN`.
    pub token: Option<String>,
    #[serde(skip)]
    path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            registry: DEFAULT_REGISTRY.to_string(),
            color: true,
            offline: false,
            jobs: 8,
            verbose: false,
            token: None,
            path: devil_home().join("config.toml"),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        devil_home().join("config.toml")
    }

    /// Load config, tolerating a missing file.
    pub fn load() -> Result<Self> {
        let path = Self::path();
        let mut cfg = if path.is_file() {
            let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
            toml_edit::de::from_str(&text).map_err(|e| Error::Toml {
                path: path.clone(),
                message: e.to_string(),
            })?
        } else {
            Config::default()
        };
        cfg.path = path;

        // Environment always wins, exactly like other cargo-family tools.
        if let Ok(v) = std::env::var("DEVIL_REGISTRY") {
            if !v.is_empty() {
                cfg.registry = v.trim_end_matches('/').to_string();
            }
        }
        if let Ok(v) = std::env::var("DEVIL_TOKEN") {
            if !v.is_empty() {
                cfg.token = Some(v);
            }
        }
        if let Ok(v) = std::env::var("DEVIL_OFFLINE") {
            cfg.offline = v == "1" || v.eq_ignore_ascii_case("true");
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path.clone();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let text = toml_edit::ser::to_string(self).map_err(|e| Error::Toml {
            path: path.clone(),
            message: e.to_string(),
        })?;
        std::fs::write(&path, text).map_err(|e| Error::io(&path, e))
    }

    /// Registry token, honouring the per-registry env var when given.
    pub fn token_for(&self, registry_token_env: Option<&str>) -> Option<String> {
        if let Some(var) = registry_token_env {
            if let Ok(tok) = std::env::var(var)
                && !tok.is_empty()
            {
                return Some(tok);
            }
        }
        self.token.clone()
    }

    pub fn credentials_path() -> PathBuf {
        devil_home().join("credentials.toml")
    }
}