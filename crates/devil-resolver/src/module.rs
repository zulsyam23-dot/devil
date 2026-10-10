//! Turning an `import` specifier into an actual file on disk.
//!
//! This is the counterpart of Node's `node_modules` walk, minus the directory
//! tree. A bare specifier is resolved through the lockfile's edge table, so
//! `chalk` imported from `a@1` and from `b@2` can land on two different
//! versions without any nesting.

use std::path::{Path, PathBuf};

use devil_core::lock::Lockfile;
use devil_core::paths::{dir_key, pkg_id, Dir, Workspace};
use devil_core::error::Error;
use devil_core::Result;

/// Modules provided by the runtime shim rather than by a package.
pub const BUILTINS: &[&str] = &[
    "assert",
    "buffer",
    "console",
    "crypto",
    "events",
    "fs",
    "http",
    "https",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "querystring",
    "stream",
    "string_decoder",
    "timers",
    "timers/promises",
    "tty",
    "url",
    "util",
    "zlib",
];

/// Extensions probed when a specifier has none, in priority order.
const EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".json", ".d.ts",
];
const INDEX_FILES: &[&str] = &[
    "index.ts",
    "index.tsx",
    "index.mts",
    "index.js",
    "index.jsx",
    "index.mjs",
    "index.cjs",
    "index.json",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A TypeScript/JavaScript file that must be compiled and inlined.
    File(PathBuf),
    /// A runtime builtin implemented by the `devil-vm` shim.
    Builtin(String),
    /// Left untouched in the output (see `[profile.release].external`).
    External(String),
}

/// Knows where every module comes from.
pub struct ModuleGraph<'a> {
    pub dir: &'a Dir,
    pub workspace: &'a Workspace,
    pub lock: &'a Lockfile,
    /// Package ids that must stay external, by name.
    pub externals: Vec<String>,
}

impl<'a> ModuleGraph<'a> {
    pub fn new(dir: &'a Dir, workspace: &'a Workspace, lock: &'a Lockfile) -> Self {
        ModuleGraph {
            dir,
            workspace,
            lock,
            externals: Vec::new(),
        }
    }

    /// Root directory of the package that owns `file`: either the workspace
    /// root, or `.devil/store/<name>-<version>`.
    pub fn owning_package_root(&self, file: &Path) -> Option<PathBuf> {
        let canonical = normalise(file);
        let root = normalise(self.dir.root());

        if canonical.starts_with(&root) {
            return Some(root);
        }
        let store = normalise(&self.dir.store());
        let rel = canonical.strip_prefix(&store).ok()?;
        let first = rel.components().next()?;
        Some(store.join(first))
    }

    /// `name@version` of the package owning `file`, if it is a dependency.
    pub fn owner_id(&self, file: &Path) -> Option<String> {
        let root = self.owning_package_root(file)?;
        let store = normalise(&self.dir.store());
        let normalised_root = normalise(&root);
        let key = normalised_root
            .strip_prefix(&store)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        split_store_key(&key).map(|(name, version)| pkg_id(&name, &version))
    }

    /// Is `spec` a runtime builtin?
    pub fn builtin(spec: &str) -> Option<String> {
        let bare = spec.strip_prefix("node:").unwrap_or(spec);
        BUILTINS
            .iter()
            .find(|b| **b == bare)
            .map(|b| (*b).to_string())
    }

    /// Resolve `spec` as imported from `importer`.
    pub fn resolve(&self, importer: &Path, spec: &str) -> Result<Target> {
        if spec.starts_with("node:") || Self::builtin(spec).is_some() {
            return Ok(Target::Builtin(spec.to_string()));
        }
        if spec.starts_with("http://")
            || spec.starts_with("https://")
            || spec.starts_with("data:")
            || spec.starts_with("//")
        {
            return Ok(Target::External(spec.to_string()));
        }

        if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
            let base = importer.parent().unwrap_or(self.dir.root());
            return resolve_file(&base.join(spec)).map(Target::File);
        }
        if spec.starts_with('/') {
            // Absolute paths are resolved against the workspace root, never the
            // filesystem root, so a bundle stays portable.
            let joined = self.dir.root().join(spec.trim_start_matches('/'));
            return resolve_file(&joined).map(Target::File);
        }

        let (pkg, subpath) = split_specifier(spec);
        if self.externals.iter().any(|e| e == &pkg) {
            return Ok(Target::External(spec.to_string()));
        }

        let owner = self.owner_id(importer).unwrap_or_else(|| self.workspace.id());
        let version = self
            .lock
            .edge(&owner, &pkg)
            .map(str::to_string)
            .or_else(|| {
                // The root package may depend on something the lockfile stores
                // under a different spelling; fall back to any version present.
                self.lock
                    .graph
                    .get(&self.workspace.id())
                    .and_then(|m| m.get(&pkg))
                    .cloned()
            })
            .ok_or_else(|| {
                Error::custom(format!(
                    "cannot resolve `{spec}` from {}: `{pkg}` is not a dependency in `{}`\n  hint: run `devil add {pkg}`",
                    display(importer),
                    self.dir.lock_path().display()
                ))
            })?;

        let pkg_dir = self.dir.package_dir(&pkg, &version);
        if !pkg_dir.is_dir() {
            return Err(Error::custom(format!(
                "package `{pkg}@{version}` is not installed\n  expected it at {}\n  hint: run `devil install`",
                pkg_dir.display()
            )));
        }

        if subpath.is_empty() {
            let meta = devil_package::read_meta(&self
                .dir
                .package_file(&pkg, &version))?;
            let entry = meta.entry.clone().unwrap_or_else(|| "src/index.ts".into());
            return resolve_file(&pkg_dir.join(entry)).map(Target::File);
        }

        // Try the declared layout first, then the `src/` convention.
        for candidate in [
            pkg_dir.join(&subpath),
            pkg_dir.join("src").join(&subpath),
        ] {
            if let Ok(path) = resolve_file(&candidate) {
                return Ok(Target::File(path));
            }
        }

        Err(Error::custom(format!(
            "cannot resolve `{spec}` inside `{pkg}@{version}`\n  looked in {}\n  tried extensions: {}",
            pkg_dir.display(),
            EXTENSIONS.join(", ")
        )))
    }

    /// Every dependency present in the store, for diagnostics.
    pub fn installed(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.dir.store()) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let key = entry.file_name().to_string_lossy().replace('\\', "/");
            if let Some((name, version)) = split_store_key(&key) {
                out.push((pkg_id(&name, &version), path));
            }
        }
        out.sort();
        out
    }
}

/// Split `name-version` where the version starts with a digit, so package
/// names containing dashes stay intact.
fn split_store_key(key: &str) -> Option<(String, String)> {
    for (i, ch) in key.char_indices() {
        if ch == '-' && key[i + 1..].starts_with(|c: char| c.is_ascii_digit()) {
            return Some((key[..i].to_string(), key[i + 1..].to_string()));
        }
    }
    None
}

/// `@scope/name` support is intentionally absent, but keep the splitter
/// honest about the `name/sub/path` shape.
pub fn split_specifier(spec: &str) -> (String, String) {
    match spec.find('/') {
        Some(idx) => (spec[..idx].to_string(), spec[idx + 1..].to_string()),
        None => (spec.to_string(), String::new()),
    }
}

/// Probe a base path until something exists on disk.
pub fn resolve_file(base: &Path) -> Result<PathBuf> {
    let base = normalise(base);
    if base.is_file() {
        return Ok(base);
    }
    for ext in EXTENSIONS {
        let candidate = with_extension(&base, ext);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if base.is_dir() {
        for index in INDEX_FILES {
            let candidate = base.join(index);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // `./foo.js` may be written in TypeScript sources as `./foo.ts`.
    if let Some(stem) = base.file_name().and_then(|s| s.to_str()) {
        for ext in EXTENSIONS {
            if stem.ends_with(ext) {
                let trimmed = &stem[..stem.len() - ext.len()];
                let candidate = base.with_file_name(format!("{trimmed}.ts"));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(Error::custom(format!(
        "file not found: {}",
        base.display()
    )))
}

fn with_extension(base: &Path, ext: &str) -> PathBuf {
    if base.is_file() {
        return base.to_path_buf();
    }
    let mut name = base.file_name().unwrap_or_default().to_string_lossy().into_owned();
    // `.d.ts` files are appended verbatim; others use the last dot.
    if ext == ".d.ts" {
        name.push_str(ext);
    } else if let Some(idx) = name.rfind('.') {
        name.replace_range(idx.., ext);
    } else {
        name.push_str(ext);
    }
    base.with_file_name(name)
}

pub fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn display(path: &Path) -> String {
    normalise(path).display().to_string()
}

/// Relative POSIX-style path used as a module id in the bundle.
pub fn module_id(from_root: &Path, file: &Path) -> String {
    let root = normalise(from_root);
    let file = normalise(file);
    let rel = file.strip_prefix(&root).unwrap_or(&file);
    let s = rel.to_string_lossy().replace('\\', "/");
    if let Some(rest) = s.strip_prefix(".devil/store/") {
        // Keep the `name-version` folder so two versions never collide.
        return format!(".devil/store/{rest}");
    }
    s
}

/// Directory name of a store entry: `<name>-<version>`.
pub fn store_key(name: &str, version: &str) -> String {
    dir_key(name, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_package_and_subpath() {
        assert_eq!(split_specifier("chalk"), ("chalk".into(), "".into()));
        assert_eq!(
            split_specifier("chalk/source"),
            ("chalk".into(), "source".into())
        );
    }

    #[test]
    fn strips_curdir() {
        assert_eq!(normalise(Path::new("./a/./b/../c")), PathBuf::from("a/c"));
    }
}