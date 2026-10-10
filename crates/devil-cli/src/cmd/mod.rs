//! Command implementations.

pub mod build;
pub mod deps;
pub mod init;
pub mod package;
pub mod project;
pub mod registry;
pub mod registry_server;
pub mod run;
pub mod test;

use anyhow::Result;

use devil_core::config::Config;
use devil_core::paths::Workspace;

/// Everything a command needs: the opened workspace plus global config.
pub struct Ctx {
    pub workspace: Workspace,
    pub config: Config,
}

impl Ctx {
    pub fn open() -> Result<Self> {
        let workspace = Workspace::open()?;
        let config = Config::load()?;
        Ok(Ctx { workspace, config })
    }

    pub fn name(&self) -> &str {
        self.workspace.name()
    }

    pub fn version(&self) -> String {
        self.workspace.version().to_string()
    }

    /// Registry this package publishes to / resolves from by default.
    ///
    /// An explicit `publish = "<url>"` in `devil.toml` wins; otherwise the
    /// global config applies, so `DEVIL_REGISTRY` or `devil login`-style
    /// settings can point a package at a private mirror.
    pub fn registry(&self) -> String {
        let explicit = self
            .workspace
            .manifest
            .package
            .publish
            .as_ref()
            .and_then(|p| p.registry.clone());
        match explicit {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => self.config.registry.trim_end_matches('/').to_string(),
        }
    }
}