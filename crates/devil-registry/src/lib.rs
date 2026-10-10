//! The registry protocol: index, download, publish, search, yank.
//!
//! A registry is any HTTP server that speaks these four endpoints. The layout
//! is deliberately static so it can be served from a CDN or a bare object
//! store, with no database query language involved.
//!
//! ```text
//! GET  {base}/index/{name}              -> PackageIndex   (JSON)
//! GET  {base}/dl/{name}/{version}        -> <name>-<ver>.devil  (octet-stream)
//! GET  {base}/api/v1/search?q=...        -> SearchResponse (JSON)
//! POST {base}/api/v1/publish            -> PublishResponse (JSON)
//! POST {base}/api/v1/yank               -> 200 (JSON)
//! ```

use std::collections::BTreeMap;

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use semver::Version;

use devil_core::manifest::LockedDependency;
use devil_core::util::join_url;
use devil_core::error::Error;
use devil_core::Result;

const USER_AGENT: &str = concat!("devil/", env!("CARGO_PKG_VERSION"));
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// One published version of a package.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct IndexEntry {
    pub name: String,
    pub version: String,
    /// `sha256-<hex>` of the `.devil` archive bytes.
    pub cksum: String,
    #[serde(default)]
    pub deps: Vec<LockedDependency>,
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

impl IndexEntry {
    pub fn parsed_version(&self) -> Result<Version> {
        Version::parse(&self.version).map_err(|source| Error::Version {
            input: self.version.clone(),
            source,
        })
    }

    /// Infallible variant for sorting; malformed versions sort as `0.0.0`.
    pub fn v(&self) -> Version {
        self.parsed_version().unwrap_or_else(|_| Version::new(0, 0, 0))
    }
}

/// The full index document for one package name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PackageIndex {
    pub name: String,
    #[serde(default)]
    pub versions: BTreeMap<String, IndexEntry>,
    /// Optional marker so a registry can serve a 304-style empty answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl PackageIndex {
    pub fn empty(name: &str) -> Self {
        PackageIndex {
            name: name.to_string(),
            versions: BTreeMap::new(),
            updated_at: None,
        }
    }

    /// Non-yanked versions, newest first.
    pub fn versions_sorted(&self) -> Vec<IndexEntry> {
        let mut out: Vec<IndexEntry> = self.versions.values().cloned().collect();
        out.sort_by(|a, b| b.v().cmp(&a.v()));
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SearchHit {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub downloads: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SearchResponse {
    #[serde(default)]
    pub results: Vec<SearchHit>,
    #[serde(default)]
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PublishRequest {
    pub name: String,
    pub version: String,
    pub cksum: String,
    pub size: u64,
    pub description: Option<String>,
    pub readme: Option<String>,
    /// JSON encoded dependency list, mirrors the embedded manifest.
    pub deps: serde_json::Value,
    pub yanked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PublishResponse {
    pub ok: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct YankRequest {
    pub name: String,
    pub version: String,
    pub yanked: bool,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------

/// A handle on one registry.
#[derive(Debug, Clone)]
pub struct Registry {
    base: String,
    token: Option<String>,
    timeout: Duration,
    offline: bool,
}

impl Registry {
    pub fn new(base: impl Into<String>) -> Self {
        Registry {
            base: base.into().trim_end_matches('/').to_string(),
            token: None,
            timeout: DEFAULT_TIMEOUT,
            offline: false,
        }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn agent(&self) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .build()
            .into()
    }

    fn require_online(&self) -> Result<()> {
        if self.offline {
            return Err(Error::custom(format!(
                "offline mode is enabled, cannot reach `{}`\n  hint: unset DEVIL_OFFLINE or run without --offline",
                self.base
            )));
        }
        Ok(())
    }

    // -- index ------------------------------------------------------------

    pub fn index_url(&self, name: &str) -> String {
        join_url(&self.base, &format!("index/{name}"))
    }

    /// Fetch the index of a package. A 404 becomes an empty index so the
    /// caller can produce a proper "no matching version" message.
    pub fn index(&self, name: &str) -> Result<PackageIndex> {
        self.require_online()?;
        let url = self.index_url(name);
        let mut req = self.agent().get(&url);
        req = req.header("Accept", "application/json");
        req = req.header("User-Agent", USER_AGENT);
        if let Some(token) = &self.token {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }

        match req.call() {
            Ok(resp) => {
                let body = resp
                    .into_body()
                    .read_to_string()
                    .map_err(|e| Error::Http {
                        url: url.clone(),
                        message: e.to_string(),
                    })?;
                serde_json::from_str(&body).map_err(|e| Error::Http {
                    url: url.clone(),
                    message: format!("invalid index JSON: {e}"),
                })
            }
            Err(ureq::Error::StatusCode(404)) => Ok(PackageIndex::empty(name)),
            Err(ureq::Error::StatusCode(code)) => Err(Error::HttpStatus { url, status: code }),
            Err(e) => Err(Error::Http {
                url,
                message: e.to_string(),
            }),
        }
    }

    // -- download ---------------------------------------------------------

    pub fn download_url(&self, name: &str, version: &str) -> String {
        join_url(&self.base, &format!("dl/{name}/{version}"))
    }

    /// Download a `.devil` archive into `dest`, verifying the checksum.
    pub fn download(
        &self,
        name: &str,
        version: &str,
        dest: &Path,
        expected_cksum: Option<&str>,
    ) -> Result<()> {
        if dest.is_file() {
            if let Some(sum) = expected_cksum {
                let cached = devil_core::util::sha256_file(dest)
                    .map(|h| format!("sha256-{h}"))
                    .unwrap_or_default();
                if cached == sum {
                    return Ok(());
                }
            }
        }
        self.require_online()?;

        let url = self.download_url(name, version);
        let mut req = self.agent().get(&url);
        req = req.header("User-Agent", USER_AGENT);
        if let Some(token) = &self.token {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }

        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(code)) => {
                return Err(Error::HttpStatus { url, status: code });
            }
            Err(e) => {
                return Err(Error::Http {
                    url,
                    message: e.to_string(),
                });
            }
        };

        let bytes = resp
            .into_body()
            .read_to_vec()
            .map_err(|e| Error::Http {
                url: url.clone(),
                message: e.to_string(),
            })?;

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        std::fs::write(dest, &bytes).map_err(|e| Error::io(dest, e))?;

        if let Some(sum) = expected_cksum {
            devil_package::verify(dest, sum)?;
        }
        Ok(())
    }

    // -- publish / yank / search ------------------------------------------

    pub fn publish(&self, req: &PublishRequest, archive: &Path) -> Result<PublishResponse> {
        self.require_online()?;
        let url = join_url(&self.base, "api/v1/publish");
        let metadata = serde_json::to_string(req).map_err(|e| Error::custom(e.to_string()))?;
        let form = ureq::unversioned::multipart::Form::new()
            .text("metadata", metadata.as_str())
            .file("package", archive)
            .map_err(|e| Error::io(archive, e))?;

        let mut http = self
            .agent()
            .post(&url)
            .header("User-Agent", USER_AGENT);
        if let Some(token) = &self.token {
            http = http.header("Authorization", &format!("Bearer {token}"));
        }

        let resp = match http.send(form) {
            Ok(r) => r,
            Err(ureq::Error::StatusCode(401)) | Err(ureq::Error::StatusCode(403)) => {
                return Err(Error::NoToken);
            }
            Err(ureq::Error::StatusCode(code)) => {
                return Err(Error::HttpStatus { url, status: code });
            }
            Err(e) => {
                return Err(Error::Http {
                    url,
                    message: e.to_string(),
                });
            }
        };

        let body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| Error::Http {
                url: url.clone(),
                message: e.to_string(),
            })?;
        serde_json::from_str(&body).map_err(|e| Error::Http {
            url,
            message: format!("invalid response: {e}"),
        })
    }

    pub fn yank(&self, req: &YankRequest) -> Result<()> {
        self.require_online()?;
        let url = join_url(&self.base, "api/v1/yank");
        let payload = serde_json::to_string(req).unwrap();
        let mut http = self.agent().post(&url).header("Content-Type", "application/json");
        if let Some(token) = &self.token {
            http = http.header("Authorization", &format!("Bearer {token}"));
        }
        match http.send(payload.as_bytes()) {
            Ok(_) => Ok(()),
            Err(ureq::Error::StatusCode(401)) | Err(ureq::Error::StatusCode(403)) => {
                Err(Error::NoToken)
            }
            Err(ureq::Error::StatusCode(code)) => Err(Error::HttpStatus { url, status: code }),
            Err(e) => Err(Error::Http {
                url,
                message: e.to_string(),
            }),
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<SearchResponse> {
        self.require_online()?;
        let url = format!("{}?q={}&limit={limit}", join_url(&self.base, "api/v1/search"), encode(query));
        let resp = self
            .agent()
            .get(&url)
            .header("Accept", "application/json")
            .call()
            .map_err(|e| Error::Http {
                url: url.clone(),
                message: e.to_string(),
            })?;
        let body = resp
            .into_body()
            .read_to_string()
            .map_err(|e| Error::Http {
                url: url.clone(),
                message: e.to_string(),
            })?;
        Ok(serde_json::from_str(&body).unwrap_or(SearchResponse {
            results: Vec::new(),
            total: 0,
        }))
    }
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Generate an API token locally: `dk_<32 hex>`.
pub fn generate_token() -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    let seed = format!(
        "{}:{}:{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::time::Instant::now().elapsed().as_nanos()
    );
    let mut salt = seed.into_bytes();
    if let Ok(bytes) = std::fs::read("/dev/urandom") {
        salt.extend_from_slice(&bytes[..64.min(bytes.len())]);
    }
    sha2::Digest::update(&mut hasher, &salt);
    format!("dk_{}", hex::encode(sha2::Digest::finalize(hasher))[..32].to_string())
}