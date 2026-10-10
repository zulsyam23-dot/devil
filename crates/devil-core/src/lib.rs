//! Core types shared by every `devil` crate: manifest, config, paths, errors.

pub mod config;
pub mod error;
pub mod lock;
pub mod manifest;
pub mod paths;
pub mod util;

pub use error::{Error as DevilError, Result};
pub use manifest::{
    DependencySpec, Edition, LockedDependency, Manifest, Package, Publish, RegistryOverride,
};
pub use paths::{Dir, Workspace};