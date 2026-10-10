//! `devil.lock` — the resolved graph, the equivalent of `Cargo.lock`.
//!
//! The lockfile records every exact version in the graph, plus the per-edge
//! resolution table the bundler uses to find a package directory without
//! walking a `node_modules` tree.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::Result;

pub const LOCK_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Lockfile {
    pub version: u32,
    /// Format tag so future tools can migrate.
    #[serde(default = "default_format")]
    pub format: String,
    /// `name-version -> checksum`.
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
    /// `dependent id -> {dependency name -> resolved version}`.
    #[serde(default)]
    pub graph: BTreeMap<String, BTreeMap<String, String>>,
    /// Dependencies of the root package, kept for `devil tree`.
    #[serde(default)]
    pub root: String,
}

fn default_format() -> String {
    format!("devil-lock-v{LOCK_VERSION}")
}

impl Default for Lockfile {
    fn default() -> Self {
        Lockfile {
            version: LOCK_VERSION,
            format: default_format(),
            packages: BTreeMap::new(),
            graph: BTreeMap::new(),
            root: String::new(),
        }
    }
}

impl Lockfile {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let lock: Lockfile = toml_edit::de::from_str(&text).map_err(|e| Error::Toml {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        Ok(lock)
    }

    pub fn read_or_new(path: &Path) -> Result<Self> {
        if path.is_file() { Lockfile::read(path) } else { Ok(Lockfile::default()) }
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let text = toml_edit::ser::to_string(self).map_err(|e| Error::Toml {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        std::fs::write(path, text).map_err(|e| Error::io(path, e))
    }

    pub fn checksum(&self, name: &str, version: &str) -> Option<&str> {
        self.packages
            .get(&crate::paths::dir_key(name, version))
            .map(String::as_str)
    }

    pub fn set_checksum(&mut self, name: &str, version: &str, sum: &str) {
        self.packages
            .insert(crate::paths::dir_key(name, version), sum.to_string());
    }

    /// Version of `dep_name` as seen by `dependent` (`name@version`).
    pub fn edge(&self, dependent: &str, dep_name: &str) -> Option<&str> {
        self.graph
            .get(dependent)
            .and_then(|m| m.get(dep_name))
            .map(String::as_str)
    }

    pub fn set_edge(&mut self, dependent: &str, dep_name: &str, version: &str) {
        self.graph
            .entry(dependent.to_string())
            .or_default()
            .insert(dep_name.to_string(), version.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty() && self.root.is_empty()
    }
}