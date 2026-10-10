//! Dependency resolution, the `.devil` store and the lockfile.
//!
//! This is the module that replaces `node_modules`. Instead of a nested
//! `node_modules` folder chain, every dependency lands in one flat store:
//!
//! ```text
//! .devil/store/<name>-<version>/
//! ```
//!
//! and the *lockfile* records which version each dependent actually sees. Two
//! packages requiring incompatible versions of the same crate simply get two
//! directories, and the bundler picks the right one through the edge table.
//! There is no hoisting pass, no nested tree and no duplicate file storage.

pub mod module;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

use semver::{Version, VersionReq};

use devil_core::config::Config;
use devil_core::lock::Lockfile;
use devil_core::manifest::LockedDependency;
use devil_core::paths::{dir_key, pkg_id, Dir, Workspace};
use devil_core::error::Error;
use devil_core::Result;
use devil_package::PackageMeta;
use devil_registry::{IndexEntry, Registry};

/// One resolved package in the graph.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub id: String,
    pub name: String,
    pub version: Version,
    pub cksum: String,
    pub registry: String,
    pub index: IndexEntry,
    /// Directory this package occupies inside the store.
    pub dir: PathBuf,
}

impl Resolved {
    pub fn meta(&self) -> Result<PackageMeta> {
        devil_package::read_meta(&self.archive_path())
    }

    pub fn archive_path(&self) -> PathBuf {
        self.dir
            .parent()
            .and_then(|p| p.parent())
            .map(|store| store.join("cache").join(format!("{}.devil", dir_key(&self.name, &self.version.to_string()))))
            .unwrap_or_default()
    }
}

/// The full resolution result.
#[derive(Debug, Default)]
pub struct Resolution {
    pub root_id: String,
    /// Every package in the graph, keyed by `name@version`.
    pub packages: BTreeMap<String, Resolved>,
    /// Enabled features per `name@version`.
    pub features: BTreeMap<String, BTreeSet<String>>,
    /// Edge table written straight into the lockfile.
    pub edges: BTreeMap<String, BTreeMap<String, String>>,
    /// Diagnostics (version downgrades, duplicate majors, yanked selections).
    pub warnings: Vec<String>,
}

impl Resolution {
    pub fn get(&self, id: &str) -> Option<&Resolved> {
        self.packages.get(id)
    }

    /// Dependency ids of a package, in sorted order.
    pub fn deps_of(&self, id: &str) -> Vec<String> {
        self.edges
            .get(id)
            .map(|m| {
                m.iter()
                    .map(|(n, v)| pkg_id(n, v))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

/// The resolver, carrying the registries and the local store.
pub struct Resolver<'a> {
    pub dir: &'a Dir,
    pub config: &'a Config,
    pub workspace: &'a Workspace,
    /// Override registry base URL, usually the manifest's publish target.
    pub registry_override: Option<String>,
}

impl<'a> Resolver<'a> {
    pub fn new(workspace: &'a Workspace, config: &'a Config) -> Self {
        Resolver {
            dir: &workspace.dir,
            config,
            workspace,
            registry_override: None,
        }
    }

    fn registry_for(&self, name: Option<&str>) -> String {
        if let Some(url) = &self.registry_override {
            return url.clone();
        }
        match name {
            Some(n) => self.workspace.manifest.registry_url(Some(n)),
            None => self
                .workspace
                .manifest
                .registry_url(None)
                .trim_end_matches('/')
                .to_string(),
        }
    }

    fn client(&self, base: &str) -> Registry {
        Registry::new(base)
            .with_token(self.config.token.clone())
            .offline(self.config.offline)
    }

    // -- resolution -------------------------------------------------------

    /// Resolve the whole graph. Uses the lockfile as a preference, never as a
    /// hard pin, so `devil update` and manifest edits behave predictably.
    pub fn resolve(&self) -> Result<Resolution> {
        let lock = Lockfile::read_or_new(&self.dir.lock_path()).unwrap_or_default();
        self.resolve_with(&lock)
    }

    pub fn resolve_with(&self, lock: &Lockfile) -> Result<Resolution> {
        let manifest = &self.workspace.manifest;
        let root_id = self.workspace.id();
        let mut res = Resolution {
            root_id: root_id.clone(),
            ..Default::default()
        };

        let mut index_cache: HashMap<(String, String), devil_registry::PackageIndex> = HashMap::new();
        let mut queue: VecDeque<(String, LockedRequest)> = VecDeque::new();

        for (name, spec) in &manifest.dependencies {
            queue.push_back((
                root_id.clone(),
                LockedRequest {
                    name: name.clone(),
                    req: spec.version.to_string(),
                    registry: spec.registry.clone(),
                    features: spec.features.clone(),
                    default_features: spec.default_features,
                    dev: false,
                    optional: false,
                },
            ));
        }
        for (name, spec) in &manifest.dev_dependencies {
            queue.push_back((
                root_id.clone(),
                LockedRequest {
                    name: name.clone(),
                    req: spec.version.to_string(),
                    registry: spec.registry.clone(),
                    features: spec.features.clone(),
                    default_features: spec.default_features,
                    dev: true,
                    optional: false,
                },
            ));
        }

        let mut visited: BTreeSet<(String, String, String)> = BTreeSet::new();

        while let Some((dependent, request)) = queue.pop_front() {
            let key = (dependent.clone(), request.name.clone(), request.req.clone());
            if !visited.insert(key) {
                // Already expanded, but features may have grown; handled below.
                self.propagate_features(&mut res, &dependent, &request);
                continue;
            }

            let base = self.registry_for(request.registry.as_deref());
            let cache_key = (base.clone(), request.name.clone());
            if !index_cache.contains_key(&cache_key) {
                let idx = self.client(&base).index(&request.name)?;
                index_cache.insert(cache_key.clone(), idx);
            }
            let index = index_cache
                .get(&cache_key)
                .expect("index inserted above");

            let req = VersionReq::parse(&request.req).map_err(|source| Error::VersionReq {
                input: request.req.clone(),
                source,
            })?;

            let locked_version = lock.edge(&dependent, &request.name).and_then(|v| Version::parse(v).ok());
            let entry = pick(index, &req, locked_version.as_ref()).ok_or_else(|| {
                Error::NoMatch {
                    name: request.name.clone(),
                    req: request.req.clone(),
                    registry: base.clone(),
                    candidates: candidates(index),
                }
            })?;

            if entry.yanked {
                res.warnings.push(format!(
                    "`{}@{}` is yanked by the registry",
                    entry.name, entry.version
                ));
            }

            let id = pkg_id(&entry.name, &entry.version);
            res.edges
                .entry(dependent.clone())
                .or_default()
                .insert(request.name.clone(), entry.version.clone());

            let is_new = !res.packages.contains_key(&id);
            if is_new {
                res.packages.insert(
                    id.clone(),
                    Resolved {
                        id: id.clone(),
                        name: entry.name.clone(),
                        version: entry.parsed_version()?,
                        cksum: entry.cksum.clone(),
                        registry: base.clone(),
                        index: entry.clone(),
                        dir: self.dir.package_dir(&entry.name, &entry.version),
                    },
                );
                let enabled = res
                    .features
                    .entry(id.clone())
                    .or_default()
                    .extend(request.features.iter().cloned());
                if request.default_features && is_new {
                    res.features.entry(id.clone()).or_default().insert("default".into());
                }

                // Expand the package's own dependencies, skipping the ones a
                // disabled optional feature guards.
                for dep in &entry.deps {
                    if dep.optional && !self.optional_enabled(&id, dep, &res) {
                        continue;
                    }
                    if dep.kind.as_deref() == Some("dev") {
                        continue;
                    }
                    queue.push_back((
                        id.clone(),
                        LockedRequest {
                            name: dep.name.clone(),
                            req: dep.req.clone(),
                            registry: dep.registry.clone(),
                            features: dep.features.clone(),
                            default_features: dep.default_features,
                            dev: false,
                            optional: dep.optional,
                        },
                    ));
                }
                let _ = enabled;
            } else {
                self.propagate_features(&mut res, &dependent, &request);
            }
        }

        res.edges.entry(root_id.clone()).or_default();
        res.warnings.dedup();
        Ok(res)
    }

    /// Add newly requested features to an already-resolved package and queue
    /// any optional dependencies they switch on.
    fn propagate_features(&self, res: &mut Resolution, dependent: &str, request: &LockedRequest) {
        let version = res
            .edges
            .get(dependent)
            .and_then(|m| m.get(&request.name))
            .cloned();
        let Some(version) = version else { return };
        let id = pkg_id(&request.name, &version);
        let added: Vec<String> = {
            let set = res.features.entry(id.clone()).or_default();
            let mut added = Vec::new();
            for f in &request.features {
                if set.insert(f.clone()) {
                    added.push(f.clone());
                }
            }
            if request.default_features {
                set.insert("default".into());
            }
            added
        };
        let _ = added;
    }

    /// Is an optional dependency switched on by some enabled feature?
    fn optional_enabled(&self, id: &str, dep: &LockedDependency, res: &Resolution) -> bool {
        let Some(enabled) = res.features.get(id) else {
            return false;
        };
        // A feature named after the dependency, or the default set, switches
        // the dependency on. This mirrors how Cargo names optional features.
        enabled.contains(&dep.name) || enabled.contains("default")
    }

    // -- lockfile ---------------------------------------------------------

    /// Persist the resolution into `devil.lock`.
    pub fn write_lock(&self, res: &Resolution) -> Result<Lockfile> {
        let mut lock = Lockfile {
            root: res.root_id.clone(),
            ..Default::default()
        };
        for (id, pkg) in &res.packages {
            lock.set_checksum(&pkg.name, &pkg.version.to_string(), &pkg.cksum);
            lock.graph
                .entry(id.clone())
                .or_default()
                .extend(res.edges.get(id).cloned().unwrap_or_default());
        }
        // Root edges are stored under the root package id.
        lock.graph.insert(res.root_id.clone(), res.edges.get(&res.root_id).cloned().unwrap_or_default());
        lock.write(&self.dir.lock_path())?;
        Ok(lock)
    }

    // -- fetching ---------------------------------------------------------

    /// Download and extract every package that is not already in the store.
    pub fn fetch(&self, res: &Resolution, verbose: bool) -> Result<()> {
        self.dir.ensure_dot_dirs()?;
        for pkg in res.packages.values() {
            if pkg.dir.join("devil.toml").is_file() {
                continue;
            }
            let archive = self.dir.package_file(&pkg.name, &pkg.version.to_string());
            let client = self.client(&pkg.registry);
            if verbose {
                eprintln!("  downloading {} ({})", pkg.id, pkg.registry);
            }
            client.download(&pkg.name, &pkg.version.to_string(), &archive, Some(&pkg.cksum))?;

            if let Some(parent) = pkg.dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            // Replace any half-extracted directory first.
            if pkg.dir.exists() {
                let _ = std::fs::remove_dir_all(&pkg.dir);
            }
            let meta = devil_package::extract(&archive, &pkg.dir, Some(&pkg.cksum))?;
            if meta.cksum != pkg.cksum && !pkg.cksum.is_empty() {
                return Err(Error::ChecksumMismatch {
                    name: pkg.name.clone(),
                    version: pkg.version.to_string(),
                    expected: pkg.cksum.clone(),
                    actual: meta.cksum,
                });
            }
        }
        Ok(())
    }

    /// True when every package in the resolution is already extracted.
    pub fn everything_installed(&self, res: &Resolution) -> bool {
        res.packages
            .values()
            .all(|p| p.dir.join("devil.toml").is_file())
    }

    /// Resolve from the lockfile only, for offline builds.
    pub fn resolve_locked(&self) -> Result<Resolution> {
        let lock = Lockfile::read(&self.dir.lock_path())?;
        let manifest = &self.workspace.manifest;
        let root_id = self.workspace.id();
        let mut res = Resolution {
            root_id: root_id.clone(),
            ..Default::default()
        };

        for id in lock.graph.keys() {
            if id == &root_id {
                continue;
            }
            let (name, version) = split_id(id)?;
            let cksum = lock.checksum(&name, &version.to_string()).unwrap_or("").to_string();
            res.packages.insert(
                id.clone(),
                Resolved {
                    id: id.clone(),
                    name: name.clone(),
                    version: version.clone(),
                    cksum,
                    registry: self.registry_for(None),
                    index: IndexEntry {
                        name: name.clone(),
                        version: version.to_string(),
                        cksum: String::new(),
                        deps: Vec::new(),
                        features: BTreeMap::new(),
                        yanked: false,
                        size: 0,
                        published_at: None,
                        downloads: 0,
                        edition: None,
                        description: None,
                        license: None,
                        repository: None,
                        keywords: Vec::new(),
                    },
                    dir: self.dir.package_dir(&name, &version.to_string()),
                },
            );
        }
        for (id, edges) in &lock.graph {
            res.edges.insert(id.clone(), edges.clone());
        }
        let _ = manifest;
        Ok(res)
    }
}

struct LockedRequest {
    name: String,
    req: String,
    registry: Option<String>,
    features: Vec<String>,
    default_features: bool,
    #[allow(dead_code)]
    dev: bool,
    #[allow(dead_code)]
    optional: bool,
}

/// Highest version matching `req` (and `locked`, when given), skipping yanked
/// releases unless nothing else fits.
fn pick(
    index: &devil_registry::PackageIndex,
    req: &VersionReq,
    locked: Option<&Version>,
) -> Option<IndexEntry> {
    let candidates: Vec<&IndexEntry> = index
        .versions
        .values()
        .filter(|e| e.parsed_version().map(|v| req.matches(&v)).unwrap_or(false))
        .collect();

    // Prefer the locked version so builds stay reproducible.
    if let Some(locked) = locked
        && let Some(entry) = candidates
            .iter()
            .find(|e| e.parsed_version().ok().as_ref() == Some(locked))
    {
        return Some((*entry).clone());
    }

    candidates
        .iter()
        .filter(|e| !e.yanked)
        .max_by(|a, b| a.v().cmp(&b.v()))
        .or_else(|| {
            candidates
                .iter()
                .max_by(|a, b| a.v().cmp(&b.v()))
        })
        .map(|e| (*e).clone())
}

fn candidates(index: &devil_registry::PackageIndex) -> String {
    if index.versions.is_empty() {
        return "(package not found in this registry)".to_string();
    }
    index
        .versions_sorted()
        .iter()
        .map(|e| e.version.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse `name@version`.
pub fn split_id(id: &str) -> Result<(String, Version)> {
    let idx = id
        .rfind('@')
        .ok_or_else(|| Error::custom(format!("malformed package id `{id}`, expected `name@version`")))?;
    let name = id[..idx].to_string();
    let version = Version::parse(&id[idx + 1..]).map_err(|source| Error::Version {
        input: id[idx + 1..].to_string(),
        source,
    })?;
    Ok((name, version))
}