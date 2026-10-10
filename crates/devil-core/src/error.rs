use std::path::PathBuf;

use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("I/O error: {0}")]
    PlainIo(#[from] std::io::Error),

    #[error("failed to parse {path}: {message}")]
    Toml {
        path: PathBuf,
        message: String,
    },

    #[error("failed to parse JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid semver requirement `{input}`: {source}")]
    VersionReq {
        input: String,
        #[source]
        source: semver::Error,
    },

    #[error("invalid version `{input}`: {source}")]
    Version {
        input: String,
        #[source]
        source: semver::Error,
    },

    #[error("no `devil.toml` found in {0} or any parent directory\nhint: run `devil init` to create one")]
    NoManifest(PathBuf),

    #[error("`devil.toml` is missing the required key `{0}`")]
    MissingKey(&'static str),

    #[error("package `{name}` has an invalid name: {reason}\n{name} must match {pattern}")]
    InvalidName {
        name: String,
        reason: String,
        pattern: &'static str,
    },

    #[error("failed to fetch {url}: {message}")]
    Http { url: String, message: String },

    #[error("registry returned HTTP {status} for {url}")]
    HttpStatus { url: String, status: u16 },

    #[error("checksum mismatch for {name}@{version}\n  expected: {expected}\n  actual:   {actual}\n  the downloaded package may be corrupted or tampered with")]
    ChecksumMismatch {
        name: String,
        version: String,
        expected: String,
        actual: String,
    },

    #[error("dependency `{name}@{req}` could not be resolved in registry `{registry}`\n  candidates: {candidates}")]
    NoMatch {
        name: String,
        req: String,
        registry: String,
        candidates: String,
    },

    #[error("dependency cycle detected: {0}")]
    Cycle(String),

    #[error("`devil publish` requires an API token; run `devil login` first")]
    NoToken,

    #[error("{0}")]
    Custom(String),
}

impl Error {
    pub fn custom(msg: impl Into<String>) -> Self {
        Error::Custom(msg.into())
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type AnyResult<T> = anyhow::Result<T>;