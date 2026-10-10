//! Filesystem layout. This is the part that replaces `node_modules`.
//!
//! A project on disk looks like this:
//!
//! ```text
//! my-app/
//!   devil.toml            <- manifest (like Cargo.toml)
//!   devil.lock            <- lockfile   (like Cargo.lock)
//!   .devil/
//!     cache/              <- downloaded `.devil` archives, exactly like ~/.cargo/registry/cache
//!       chalk-0.2.0.devil
//!     store/              <- extracted packages, like ~/.cargo/registry/src
//!       chalk-0.2.0/
//!     graph.json          <- resolved edges, `name -> version` per dependent
//!   src/
//!     index.ts
//! ```
//!
//! Nothing named `node_modules` is ever created.

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::Result;

pub const MANIFEST_FILE: &str = "devil.toml";
pub const LOCK_FILE: &str = "devil.lock";
pub const DOT_DIR: &str = ".devil";
pub const PACKAGE_EXT: &str = "devil";

/// Conventional source directory of a package.
pub const DEFAULT_SRC: &str = "src";
pub const DEFAULT_ENTRY: &str = "src/index.ts";

/// Marker file embedded in every `.devil` archive.
pub const EMBEDDED_MANIFEST: &str = ".devil-manifest.json";

/// Official registry used when the manifest does not override it.
pub const DEFAULT_REGISTRY: &str = "https://registry.devil-lang.org";

/// All the well-known directories inside a workspace.
#[derive(Debug, Clone)]
pub struct Dir {
    root: PathBuf,
}

impl Dir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Dir { root: root.into() }
    }

    /// Walk up from `start` until a directory containing `devil.toml` is found.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let mut cur: &Path = start.as_ref();
        loop {
            if cur.join(MANIFEST_FILE).is_file() {
                return Ok(Dir::new(cur));
            }
            match cur.parent() {
                Some(parent) => cur = parent,
                None => return Err(Error::NoManifest(start.as_ref().to_path_buf())),
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    pub fn dot(&self) -> PathBuf {
        self.root.join(DOT_DIR)
    }

    pub fn cache(&self) -> PathBuf {
        self.dot().join("cache")
    }

    pub fn store(&self) -> PathBuf {
        self.dot().join("store")
    }

    pub fn graph_path(&self) -> PathBuf {
        self.dot().join("graph.json")
    }

    pub fn vendor(&self) -> PathBuf {
        self.dot().join("vendor")
    }

    pub fn dist(&self) -> PathBuf {
        self.root.join("dist")
    }

    /// Directory holding one extracted dependency: `.devil/store/<name>-<version>`.
    pub fn package_dir(&self, name: &str, version: &str) -> PathBuf {
        self.store().join(dir_key(name, version))
    }

    /// Archive path for a downloaded package: `.devil/cache/<name>-<version>.devil`.
    pub fn package_file(&self, name: &str, version: &str) -> PathBuf {
        self.cache().join(format!("{}.{PACKAGE_EXT}", dir_key(name, version)))
    }

    pub fn ensure_dot_dirs(&self) -> Result<()> {
        for dir in [self.cache(), self.store()] {
            std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        }
        Ok(())
    }
}

/// A resolved workspace: manifest plus the directory it came from.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub dir: Dir,
    pub manifest: crate::manifest::Manifest,
}

impl Workspace {
    pub fn open() -> Result<Self> {
        Self::open_from(std::env::current_dir()?)
    }

    pub fn open_from(start: impl AsRef<Path>) -> Result<Self> {
        let dir = Dir::discover(start)?;
        let path = dir.manifest_path();
        let manifest = crate::manifest::Manifest::read(&path)?;
        Ok(Workspace { dir, manifest })
    }

    pub fn name(&self) -> &str {
        &self.manifest.package.name
    }

    pub fn version(&self) -> &semver::Version {
        &self.manifest.package.version
    }

    /// `name@version`, the canonical identity of a package.
    pub fn id(&self) -> String {
        format!("{}@{}", self.name(), self.version())
    }

    /// The absolute path of the entry module for this package.
    pub fn entry_path(&self) -> PathBuf {
        let rel = self
            .manifest
            .package
            .entry
            .clone()
            .unwrap_or_else(|| DEFAULT_ENTRY.to_string());
        self.dir.root().join(rel)
    }
}

/// Filesystem-safe identity of a package: `name-version`.
pub fn dir_key(name: &str, version: &str) -> String {
    format!("{name}-{version}")
}

/// Canonical `name@version` identity.
pub fn pkg_id(name: &str, version: &str) -> String {
    format!("{name}@{version}")
}

/// User-level config dir (`$DEVIL_HOME`, else platform config dir).
pub fn devil_home() -> PathBuf {
    if let Ok(custom) = std::env::var("DEVIL_HOME") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("devil");
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("devil");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config").join("devil")
}