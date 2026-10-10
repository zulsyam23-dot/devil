//! `devil.toml` — the package manifest, modelled on `Cargo.toml`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::paths::{DEFAULT_REGISTRY, MANIFEST_FILE};
use crate::error::Error;
use crate::Result;

// ---------------------------------------------------------------------------
// Edition
// ---------------------------------------------------------------------------

/// The language edition a package is compiled with. Like Rust's editions, it
/// decides the language target: how far syntax is downlevelled and which
/// strictness rules the type-stripping pass enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    Es2015,
    Es2017,
    Es2018,
    Es2019,
    Es2020,
    Es2021,
    Es2022,
    Es2023,
    Esnext,
}

impl Edition {
    pub const LATEST: Edition = Edition::Es2022;

    pub fn as_str(&self) -> &'static str {
        match self {
            Edition::Es2015 => "2015",
            Edition::Es2017 => "2017",
            Edition::Es2018 => "2018",
            Edition::Es2019 => "2019",
            Edition::Es2020 => "2020",
            Edition::Es2021 => "2021",
            Edition::Es2022 => "2022",
            Edition::Es2023 => "2023",
            Edition::Esnext => "next",
        }
    }

    pub fn from_str_lossy(s: &str) -> Edition {
        match s {
            "2015" => Edition::Es2015,
            "2017" => Edition::Es2017,
            "2018" => Edition::Es2018,
            "2019" => Edition::Es2019,
            "2020" => Edition::Es2020,
            "2021" => Edition::Es2021,
            "2023" => Edition::Es2023,
            "next" => Edition::Esnext,
            _ => Edition::LATEST,
        }
    }

    /// ECMA target this edition compiles down to.
    pub fn es_version_str(&self) -> &'static str {
        match self {
            Edition::Es2015 => "es2015",
            Edition::Es2017 => "es2017",
            Edition::Es2018 => "es2018",
            Edition::Es2019 => "es2019",
            Edition::Es2020 => "es2020",
            Edition::Es2021 => "es2021",
            Edition::Es2022 => "es2022",
            Edition::Es2023 => "es2023",
            Edition::Esnext => "esnext",
        }
    }
}

impl Default for Edition {
    fn default() -> Self {
        Edition::LATEST
    }
}

impl fmt::Display for Edition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Dependency spec
// ---------------------------------------------------------------------------

/// A dependency entry. Accepts the shorthand `"1.2"` as well as the full
/// inline table `{ version = "1.2", features = ["x"], default_features = false }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub version: VersionReq,
    pub registry: Option<String>,
    pub default_features: bool,
    pub features: Vec<String>,
    /// Optional git/URL source for packages that are not in any registry.
    pub source: Option<DependencySource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DependencySource {
    Git { url: String, rev: Option<String> },
    Path { path: String },
}

impl DependencySpec {
    pub fn new(req: impl Into<String>) -> Result<Self> {
        let raw: String = req.into();
        Ok(DependencySpec {
            version: parse_req(&raw)?,
            registry: None,
            default_features: true,
            features: Vec::new(),
            source: None,
        })
    }

    pub fn req_string(&self) -> String {
        self.version.to_string()
    }

    pub fn with_features(mut self, features: &[String]) -> Self {
        self.features = features.to_vec();
        self
    }
}

impl fmt::Display for DependencySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.version.to_string())
    }
}

fn parse_req(raw: &str) -> Result<VersionReq> {
    let trimmed = raw.trim();
    let normalized = if trimmed.is_empty() {
        "*".to_string()
    } else {
        trimmed.to_string()
    };
    VersionReq::parse(&normalized).map_err(|source| Error::VersionReq {
        input: raw.to_string(),
        source,
    })
}

impl<'de> Deserialize<'de> for DependencySpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case")]
        struct Req {
            version: Option<String>,
            registry: Option<String>,
            default_features: Option<bool>,
            features: Option<Vec<String>>,
            git: Option<String>,
            rev: Option<String>,
            path: Option<String>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Short(String),
            Long(Req),
        }

        let raw = match Raw::deserialize(d)? {
            Raw::Short(s) => Req {
                version: Some(s),
                registry: None,
                default_features: None,
                features: None,
                git: None,
                rev: None,
                path: None,
            },
            Raw::Long(r) => r,
        };

        let version = parse_req(raw.version.as_deref().unwrap_or("*"))
            .map_err(serde::de::Error::custom)?;

        let source = match (raw.git, raw.path) {
            (Some(url), _) => Some(DependencySource::Git { url, rev: raw.rev }),
            (None, Some(path)) => Some(DependencySource::Path { path }),
            _ => None,
        };

        Ok(DependencySpec {
            version,
            registry: raw.registry,
            default_features: raw.default_features.unwrap_or(true),
            features: raw.features.unwrap_or_default(),
            source,
        })
    }
}

impl Serialize for DependencySpec {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let is_plain = self.registry.is_none()
            && self.default_features
            && self.features.is_empty()
            && self.source.is_none();

        if is_plain {
            return s.serialize_str(&self.version.to_string());
        }

        let len = 1
            + usize::from(self.registry.is_some())
            + usize::from(!self.default_features)
            + usize::from(!self.features.is_empty())
            + usize::from(self.source.is_some());
        let mut map = s.serialize_map(Some(len))?;
        map.serialize_entry("version", &self.version.to_string())?;
        if let Some(r) = &self.registry {
            map.serialize_entry("registry", r)?;
        }
        if !self.default_features {
            map.serialize_entry("default_features", &false)?;
        }
        if !self.features.is_empty() {
            map.serialize_entry("features", &self.features)?;
        }
        match &self.source {
            Some(DependencySource::Git { url, rev }) => {
                map.serialize_entry("git", url)?;
                if let Some(rev) = rev {
                    map.serialize_entry("rev", rev)?;
                }
            }
            Some(DependencySource::Path { path }) => {
                map.serialize_entry("path", path)?;
            }
            None => {}
        }
        map.end()
    }
}

// ---------------------------------------------------------------------------
// Package
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Package {
    pub name: String,
    pub version: Version,
    #[serde(default)]
    pub edition: Edition,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub documentation: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub readme: Option<String>,
    /// Entry module, relative to the manifest. Defaults to `src/index.ts`.
    #[serde(default)]
    pub entry: Option<String>,
    /// Extra globs to ship inside the `.devil` archive.
    #[serde(default)]
    pub include: Vec<String>,
    /// Directories/files never shipped inside the archive.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub publish: Option<Publish>,
}

/// `publish = false` or `publish = { registry = "..." }`.
#[derive(Debug, Clone)]
pub struct Publish {
    pub enabled: bool,
    pub registry: Option<String>,
}

impl Default for Publish {
    fn default() -> Self {
        Publish {
            enabled: true,
            registry: None,
        }
    }
}

impl Publish {
    pub fn allowed(&self) -> bool {
        self.enabled
    }
}

impl Serialize for Publish {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match &self.registry {
            None => s.serialize_bool(self.enabled),
            Some(url) => s.serialize_str(url),
        }
    }
}

impl<'de> Deserialize<'de> for Publish {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case")]
        struct Cfg {
            publish: Option<bool>,
            registry: Option<String>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Flag(bool),
            Url(String),
            Table(Cfg),
        }

        let (enabled, registry) = match Raw::deserialize(d)? {
            Raw::Flag(b) => (b, None),
            Raw::Url(u) => (true, Some(u)),
            Raw::Table(c) => (c.publish.unwrap_or(true), c.registry),
        };
        Ok(Publish { enabled, registry })
    }
}

/// Alternative registry target, referenced by name from `[registries]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RegistryOverride {
    pub registry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BinaryTarget {
    pub name: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Manifest {
    pub package: Package,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub dev_dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub peer_dependencies: BTreeMap<String, DependencySpec>,
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub registries: BTreeMap<String, RegistryOverride>,
    #[serde(default)]
    pub bin: Vec<BinaryTarget>,
    /// `[profile.<name>]` — build tuning knobs.
    #[serde(default)]
    pub profile: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Profile {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub minify: Option<bool>,
    #[serde(default)]
    pub sourcemap: Option<bool>,
    #[serde(default)]
    pub external: Vec<String>,
    /// Inline these package names into the bundle instead of leaving them external.
    #[serde(default)]
    pub inline: Vec<String>,
}

impl Manifest {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Self::parse(&text).map_err(|e| match e {
            Error::Toml { message, .. } => Error::Toml {
                path: path.to_path_buf(),
                message,
            },
            other => other,
        })
    }

    pub fn parse(text: &str) -> Result<Self> {
        toml_edit::de::from_str(text).map_err(|e| Error::Toml {
            path: PathBuf::from(MANIFEST_FILE),
            message: e.to_string(),
        })
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

    /// Normal dependencies plus dev dependencies, dev taking lower priority.
    pub fn all_dependencies(&self) -> BTreeMap<String, &DependencySpec> {
        let mut out: BTreeMap<String, &DependencySpec> = BTreeMap::new();
        for (k, v) in &self.dev_dependencies {
            out.insert(k.clone(), v);
        }
        for (k, v) in &self.dependencies {
            out.insert(k.clone(), v);
        }
        out
    }

    /// Resolve a registry name (or default) to a base URL.
    pub fn registry_url(&self, name: Option<&str>) -> String {
        match name {
            None => DEFAULT_REGISTRY.to_string(),
            Some(name) => match self.registries.get(name) {
                Some(r) => trim_slash(&r.registry),
                None => name.to_string(),
            },
        }
    }

    pub fn publish_registry(&self) -> String {
        match self
            .package
            .publish
            .as_ref()
            .and_then(|p| p.registry.clone())
        {
            Some(url) => trim_slash(&url),
            None => DEFAULT_REGISTRY.to_string(),
        }
    }
}

fn trim_slash(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// Locked form (stored inside a `.devil` archive)
// ---------------------------------------------------------------------------

/// The dependency list stored in the embedded `.devil-manifest.json` so the
/// resolver never has to re-parse `devil.toml` out of an archive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LockedDependency {
    pub name: String,
    pub req: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(default)]
    pub default_features: bool,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional dependencies are only pulled in when a feature enables them.
    #[serde(default)]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DependencySource>,
}

impl From<(&str, &DependencySpec)> for LockedDependency {
    fn from((name, spec): (&str, &DependencySpec)) -> Self {
        LockedDependency {
            name: name.to_string(),
            req: spec.version.to_string(),
            registry: spec.registry.clone(),
            default_features: spec.default_features,
            features: spec.features.clone(),
            kind: None,
            optional: false,
            source: spec.source.clone(),
        }
    }
}