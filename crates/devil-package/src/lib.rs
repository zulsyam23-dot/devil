//! The `.devil` archive format — a gzipped tar, exactly like Cargo's `.crate`.
//!
//! ```text
//! chalk-0.2.0.devil  (gzip)
//!   └── chalk-0.2.0/            <- single top-level directory, like .crate
//!       ├── devil.toml         <- the manifest, normalised
//!       ├── .devil-manifest.json <- metadata used by the resolver
//!       ├── README.md
//!       └── src/*.ts
//! ```
//!
//! Archives are written **deterministically**: entries are sorted, timestamps
//! and ownership are zeroed and permissions are normalised, so packing the same
//! tree twice always yields byte-identical output and therefore the same
//! `sha256-<hex>` checksum.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use devil_core::manifest::{DependencySpec, Edition, LockedDependency, Manifest};
use devil_core::paths::EMBEDDED_MANIFEST;
use devil_core::util::checksum;
use devil_core::error::Error;
use devil_core::Result;

/// Epoch used for every archive member: 1980-01-01, the portable "no time".
const FIXED_MTIME: u64 = 315532800;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PackageMeta {
    pub name: String,
    pub version: String,
    pub edition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<LockedDependency>,
    #[serde(default)]
    pub dev_dependencies: Vec<LockedDependency>,
    #[serde(default)]
    pub peer_dependencies: Vec<LockedDependency>,
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub bin: BTreeMap<String, String>,
    /// Number of files inside the package.
    #[serde(default)]
    pub file_count: usize,
    /// Size of the source tree before compression.
    #[serde(default)]
    pub unpacked_size: u64,
    /// Checksum over the packed member list + contents (not the gzip bytes).
    #[serde(default)]
    pub cksum: String,
    /// Registry that hosts this package, informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
}

impl PackageMeta {
    pub fn from_manifest(m: &Manifest) -> Self {
        let to_locked = |map: &BTreeMap<String, DependencySpec>| -> Vec<LockedDependency> {
            map.iter()
                .map(|(name, spec)| LockedDependency::from((name.as_str(), spec)))
                .collect()
        };

        PackageMeta {
            name: m.package.name.clone(),
            version: m.package.version.to_string(),
            edition: m.package.edition.as_str().to_string(),
            description: m.package.description.clone(),
            authors: m.package.authors.clone(),
            license: m.package.license.clone(),
            repository: m.package.repository.clone(),
            homepage: m.package.homepage.clone(),
            keywords: m.package.keywords.clone(),
            categories: m.package.categories.clone(),
            entry: m.package.entry.clone(),
            dependencies: to_locked(&m.dependencies),
            dev_dependencies: to_locked(&m.dev_dependencies),
            peer_dependencies: to_locked(&m.peer_dependencies),
            features: m.features.clone(),
            bin: m
                .bin
                .iter()
                .map(|b| (b.name.clone(), b.path.clone()))
                .collect(),
            file_count: 0,
            unpacked_size: 0,
            cksum: String::new(),
            registry: None,
        }
    }

    pub fn id(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    pub fn edition(&self) -> Edition {
        Edition::from_str_lossy(&self.edition)
    }

    /// All deps the resolver has to walk (dev deps included).
    pub fn all_dependencies(&self) -> impl Iterator<Item = &LockedDependency> {
        self.dependencies
            .iter()
            .chain(self.dev_dependencies.iter())
            .chain(self.peer_dependencies.iter())
    }
}

/// Result of packing a directory.
#[derive(Debug, Clone)]
pub struct PackInfo {
    pub path: PathBuf,
    pub meta: PackageMeta,
    pub archive_checksum: String,
    pub archive_size: u64,
}

/// Pack `root` (a directory containing `devil.toml`) into a `.devil` archive.
pub fn pack(root: &Path, manifest: &Manifest, out: Option<&Path>) -> Result<PackInfo> {
    let name = &manifest.package.name;
    let version = manifest.package.version.to_string();
    devil_core::util::validate_name(name)?;

    let out = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let parent = root.parent().unwrap_or(Path::new("."));
            parent.join(format!(
                "{}-{version}.{}",
                name,
                devil_core::paths::PACKAGE_EXT
            ))
        }
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }

    let members = collect_members(root, manifest)?;

    let meta = PackageMeta::from_manifest(manifest);
    let mut meta = meta;
    meta.file_count = members.len();
    meta.unpacked_size = members.iter().map(|m| m.len as u64).sum();
    meta.cksum = tree_checksum(&members, &meta)?;

    // Normalise the manifest that travels inside the archive so a consumer
    // never sees a dev-only or path-only dependency.
    let normalised = normalise_manifest(manifest);
    let manifest_bytes = toml_edit::ser::to_string(&normalised)
        .map_err(|e| Error::Toml {
            path: root.join(devil_core::paths::MANIFEST_FILE),
            message: e.to_string(),
        })?
        .into_bytes();
    let meta_bytes = serde_json::to_vec_pretty(&meta)?;

    let file = File::create(&out).map_err(|e| Error::io(&out, e))?;
    let gz = GzEncoder::new(BufWriter::new(file), Compression::best());
    let mut ar = tar::Builder::new(gz);
    ar.mode(tar::HeaderMode::Deterministic);

    let prefix = format!("{name}-{version}");

    append_bytes(&mut ar, &format!("{prefix}/devil.toml"), &manifest_bytes)?;
    append_bytes(
        &mut ar,
        &format!("{prefix}/{EMBEDDED_MANIFEST}"),
        &meta_bytes,
    )?;

    for m in &members {
        let bytes = std::fs::read(&m.abs).map_err(|e| Error::io(&m.abs, e))?;
        append_bytes(&mut ar, &format!("{prefix}/{}", m.rel), &bytes)?;
    }

    let gz = ar
        .into_inner()
        .map_err(|e| Error::custom(format!("failed to finalise archive: {e}")))?;
    let file = gz
        .finish()
        .map_err(|e| Error::custom(format!("failed to finalise gzip stream: {e}")))?;
    let mut file = file;
    file.flush().map_err(|e| Error::io(&out, e))?;
    drop(file);

    let archive_bytes = std::fs::read(&out).map_err(|e| Error::io(&out, e))?;
    Ok(PackInfo {
        archive_size: archive_bytes.len() as u64,
        archive_checksum: checksum(&archive_bytes),
        path: out,
        meta,
    })
}

struct Member {
    rel: String,
    abs: PathBuf,
    len: usize,
}

fn append_bytes<W: Write>(ar: &mut tar::Builder<W>, path: &str, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(FIXED_MTIME);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    ar.append_data(&mut header, path, data)
        .map_err(|e| Error::custom(format!("tar append failed: {e}")))
}

/// Decide which files ship, and in what order.
fn collect_members(root: &Path, manifest: &Manifest) -> Result<Vec<Member>> {
    let mut always_ignored = vec![
        ".devil".to_string(),
        ".git".to_string(),
        "devil.lock".to_string(),
        "dist".to_string(),
        "node_modules".to_string(),
    ];
    always_ignored.extend(manifest.package.exclude.iter().cloned());

    let mut include: Vec<String> = manifest.package.include.clone();
    if include.is_empty() {
        include = vec!["src".to_string()];
        if let Some(readme) = &manifest.package.readme
            && include.iter().all(|p| p != readme)
        {
            include.push(readme.clone());
        }
        include.push("LICENSE".into());
        include.push("LICENSE.md".into());
        include.push("LICENSE.txt".into());
        include.push("README.md".into());
    }

    let mut members = Vec::new();
    let walker = WalkDir::new(root).follow_links(false).into_iter();
    for entry in walker.filter_entry(|e| !is_ignored(root, e.path(), &always_ignored)) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => return Err(Error::custom(format!("walk error: {e}"))),
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = rel_string(root, entry.path());
        let matches = include
            .iter()
            .any(|pattern| glob_match(pattern, &rel) || has_prefix(&rel, pattern));
        if !matches {
            continue;
        }
        let len = entry.metadata().map(|m| m.len() as usize).unwrap_or(0);
        members.push(Member {
            rel,
            abs: entry.path().to_path_buf(),
            len,
        });
    }

    members.sort_by(|a, b| a.rel.cmp(&b.rel));

    if members.is_empty() {
        return Err(Error::custom(format!(
            "no files matched the include patterns of `{}`; add files or set `package.include`",
            manifest.package.name
        )));
    }
    Ok(members)
}

fn has_prefix(rel: &str, pattern: &str) -> bool {
    rel == pattern || rel.starts_with(&format!("{pattern}/"))
}

fn is_ignored(root: &Path, path: &Path, patterns: &[String]) -> bool {
    if path == root {
        return false;
    }
    let rel = rel_string(root, path);
    rel.split('/').any(|seg| {
        patterns.iter().any(|p| glob_match(p, &rel) || p == seg || rel == *p)
    })
}

fn rel_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Checksum over the deterministic member list: name, length and content.
fn tree_checksum(members: &[Member], meta: &PackageMeta) -> Result<String> {
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut hasher, meta.name.as_bytes());
    sha2::Digest::update(&mut hasher, b"@");
    sha2::Digest::update(&mut hasher, meta.version.as_bytes());
    for m in members {
        sha2::Digest::update(&mut hasher, m.rel.as_bytes());
        let bytes = std::fs::read(&m.abs).map_err(|e| Error::io(&m.abs, e))?;
        sha2::Digest::update(&mut hasher, &bytes);
    }
    Ok(format!(
        "sha256-{}",
        hex::encode(sha2::Digest::finalize(hasher))
    ))
}

fn normalise_manifest(m: &Manifest) -> Manifest {
    let mut m = m.clone();
    m.dev_dependencies.clear();
    m.peer_dependencies
        .retain(|_, v| v.source.is_some());
    m
}

/// Read the embedded metadata without extracting anything.
pub fn read_meta(archive: &Path) -> Result<PackageMeta> {
    let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
    let gz = GzDecoder::new(BufReader::new(file));
    let mut ar = tar::Archive::new(gz);

    for entry in ar
        .entries()
        .map_err(|e| Error::custom(format!("corrupt archive: {e}")))?
    {
        let mut entry = entry.map_err(|e| Error::custom(format!("corrupt archive: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| Error::custom(format!("corrupt entry path: {e}")))?
            .to_string_lossy()
            .into_owned();
        if path.ends_with(EMBEDDED_MANIFEST) {
            let mut buf = String::new();
            entry
                .read_to_string(&mut buf)
                .map_err(|e| Error::custom(format!("corrupt metadata: {e}")))?;
            return serde_json::from_str(&buf)
                .map_err(|e| Error::custom(format!("corrupt metadata: {e}")));
        }
    }
    Err(Error::custom(format!(
        "not a .devil package: `{EMBEDDED_MANIFEST}` is missing from {}",
        archive.display()
    )))
}

/// Verify the archive against a `sha256-<hex>` checksum.
pub fn verify(archive: &Path, expected: &str) -> Result<()> {
    let bytes = std::fs::read(archive).map_err(|e| Error::io(archive, e))?;
    let actual = checksum(&bytes);
    if actual != expected {
        let meta = read_meta(archive).ok();
        let name = meta.as_ref().map(|m| m.name.clone()).unwrap_or_default();
        let version = meta.as_ref().map(|m| m.version.clone()).unwrap_or_default();
        return Err(Error::ChecksumMismatch {
            name,
            version,
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Extract an archive into `dest`. The archive is verified first.
pub fn extract(archive: &Path, dest: &Path, expected_checksum: Option<&str>) -> Result<PackageMeta> {
    if let Some(sum) = expected_checksum {
        verify(archive, sum)?;
    }

    let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
    let gz = GzDecoder::new(BufReader::new(file));
    let mut ar = tar::Archive::new(gz);
    ar.set_overwrite(true);
    // `.crate` archives carry a single top level dir; strip it so the store
    // layout stays `.devil/store/<name>-<version>/`.
    let root = archive_prefix(archive)?;

    let mut meta: Option<PackageMeta> = None;

    for entry in ar
        .entries()
        .map_err(|e| Error::custom(format!("corrupt archive: {e}")))?
    {
        let mut entry = entry.map_err(|e| Error::custom(format!("corrupt entry: {e}")))?;
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            continue;
        }
        let raw = entry
            .path()
            .map_err(|e| Error::custom(format!("corrupt entry path: {e}")))?
            .into_owned();

        let stripped = strip_prefix_path(&raw, &root);
        if stripped.as_os_str().is_empty() {
            continue;
        }
        // Reject traversal attempts (`../`).
        if stripped
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(Error::custom(format!(
                "refusing to extract `{}`: path escapes the destination",
                raw.display()
            )));
        }

        if let Some(name) = stripped.file_name().and_then(|s| s.to_str())
            && stripped.components().count() == 1
            && name == EMBEDDED_MANIFEST
        {
            let mut buf = String::new();
            entry
                .read_to_string(&mut buf)
                .map_err(|e| Error::custom(format!("corrupt metadata: {e}")))?;
            meta = Some(
                serde_json::from_str(&buf)
                    .map_err(|e| Error::custom(format!("corrupt metadata: {e}")))?,
            );
        }

        let out = dest.join(&stripped);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        entry
            .unpack(&out)
            .map_err(|e| Error::custom(format!("failed to unpack {}: {e}", raw.display())))?;
    }

    meta.ok_or_else(|| {
        Error::custom(format!(
            "not a .devil package: `{EMBEDDED_MANIFEST}` is missing from {}",
            archive.display()
        ))
    })
}

/// Read the single top-level directory of an archive.
fn archive_prefix(archive: &Path) -> Result<String> {
    let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
    let gz = GzDecoder::new(BufReader::new(file));
    let mut ar = tar::Archive::new(gz);
    let first = ar
        .entries()
        .map_err(|e| Error::custom(format!("corrupt archive: {e}")))?
        .next()
        .ok_or_else(|| Error::custom(format!("empty archive: {}", archive.display())))?
        .map_err(|e| Error::custom(format!("corrupt archive: {e}")))?;
    let path = first
        .path()
        .map_err(|e| Error::custom(format!("corrupt archive: {e}")))?
        .into_owned();
    match path.components().next() {
        Some(Component::Normal(first)) => Ok(first.to_string_lossy().into_owned()),
        _ => Err(Error::custom(format!(
            "archive {} must contain a single top-level directory",
            archive.display()
        ))),
    }
}

fn strip_prefix_path(path: &Path, prefix: &str) -> PathBuf {
    let mut out = PathBuf::new();
    let mut matched = false;
    for c in path.components() {
        match c {
            Component::Normal(seg) => {
                if !matched {
                    matched = seg.to_string_lossy() == prefix;
                    continue;
                }
                out.push(seg);
            }
            _ => {}
        }
    }
    if matched { out } else { path.to_path_buf() }
}

// ---------------------------------------------------------------------------
// Minimal glob matcher: `*`, `**`, `?`
// ---------------------------------------------------------------------------

pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_at(&p, 0, &t, 0)
}

fn glob_at(p: &[char], mut pi: usize, t: &[char], mut ti: usize) -> bool {
    while pi < p.len() {
        match p[pi] {
            '*' => {
                let double = p.get(pi + 1) == Some(&'*');
                let next = if double { pi + 2 } else { pi + 1 };
                // `**/` also matches zero directories.
                if double {
                    if p.get(next) == Some(&'/') {
                        if glob_at(p, next + 1, t, ti) {
                            return true;
                        }
                    }
                    let mut k = ti;
                    loop {
                        if glob_at(p, next, t, k) {
                            return true;
                        }
                        if k >= t.len() {
                            return false;
                        }
                        k += 1;
                    }
                }
                let mut k = ti;
                loop {
                    if glob_at(p, next, t, k) {
                        return true;
                    }
                    if k >= t.len() || t[k] == '/' {
                        return false;
                    }
                    k += 1;
                }
            }
            '?' => {
                if ti >= t.len() || t[ti] == '/' {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti >= t.len() || t[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == t.len()
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn globs() {
        assert!(glob_match("src", "src"));
        assert!(glob_match("**/*.ts", "src/a/b.ts"));
        assert!(glob_match("**/*.ts", "a.ts"));
        assert!(glob_match("src/*.ts", "src/a.ts"));
        assert!(!glob_match("src/*.ts", "src/a/b.ts"));
        assert!(glob_match("README.md", "README.md"));
        assert!(glob_match("*.txt", "a.txt"));
    }
}