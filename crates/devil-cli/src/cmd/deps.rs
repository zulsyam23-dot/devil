//! Dependency commands: `add`, `remove`, `update`, `install`, `sync`,
//! `vendor`, `clean`, `tree`, `info`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use toml_edit::{DocumentMut, Item, Table, Value};

use super::build::to_anyhow;
use super::Ctx;
use crate::ui;
use devil_core::lock::Lockfile;
use devil_core::util::{split_name_version, validate_name};
use devil_core::paths::MANIFEST_FILE;
use devil_resolver::Resolver;

// ---------------------------------------------------------------------------
// add
// ---------------------------------------------------------------------------

#[derive(Debug, clap::Args)]
pub struct Add {
    /// Packages to add, optionally `name@version` or `name@^1.2`.
    #[arg(required = true)]
    pub packages: Vec<String>,
    /// Add to `[dev-dependencies]`.
    #[arg(long)]
    pub dev: bool,
    /// Comma separated list of features.
    #[arg(long, value_delimiter = ',')]
    pub features: Vec<String>,
    /// Do not install, only edit `devil.toml`.
    #[arg(long)]
    pub no_install: bool,
    /// Save the exact version instead of the caret range.
    #[arg(long)]
    pub exact: bool,
}

pub fn add(ctx: &Ctx, args: Add) -> Result<()> {
    let manifest_path = ctx.workspace.dir.manifest_path();
    let mut doc = read_manifest(&manifest_path)?;

    let table_name = if args.dev {
        "dev-dependencies"
    } else {
        "dependencies"
    };

    let mut requested: Vec<(String, String)> = Vec::new();
    for spec in &args.packages {
        let (name, version) = split_name_version(spec);
        validate_name(&name)?;
        requested.push((name, version.unwrap_or_else(|| "*".to_string())));
    }

    // Ask the registry which version is current, then write a caret range.
    let resolver = Resolver::new(&ctx.workspace, &ctx.config);
    for (name, req) in &requested {
        let index = devil_registry::Registry::new(ctx.registry())
            .with_token(ctx.config.token.clone())
            .offline(ctx.config.offline)
            .index(name)
            .map_err(to_anyhow)?;
        let resolved = if req == "*" {
            index
                .versions_sorted()
                .into_iter()
                .find(|e| !e.yanked)
                .map(|e| e.version)
        } else {
            devil_registry::Registry::new(ctx.registry())
                .index(name)
                .ok()
                .and_then(|i| {
                    let r: Result<semver::VersionReq, _> = req.parse();
                    r.ok().and_then(|r| {
                        i.versions_sorted()
                            .into_iter()
                            .find(|e| semver::Version::parse(&e.version).map(|v| r.matches(&v)).unwrap_or(false))
                            .map(|e| e.version)
                    })
                })
        };

        let resolved = match resolved {
            Some(v) => v,
            None if req == "*" => {
                return Err(anyhow::anyhow!(
                    "package `{name}` was not found in {}\n  is the registry correct?",
                    ctx.registry()
                ));
            }
            None => return Err(anyhow::anyhow!("no version of `{name}` matches `{req}`")),
        };

        let range = if args.exact {
            format!("={resolved}")
        } else {
            caret(&resolved)
        };

        set_dependency(&mut doc, table_name, name, &range, &args.features)?;
        ui::status(
            ui::green("adding"),
            format!("{} {}", name, dim_range(&range, &resolved)),
        );
        let _ = &resolver;
    }

    doc.to_string();
    std::fs::write(&manifest_path, doc.to_string().as_bytes())?;
    ui::ok("updated `devil.toml`");

    if args.no_install {
        return Ok(());
    }
    install(ctx, Install { lock: false, offline: ctx.config.offline })
}

fn dim_range(range: &str, resolved: &str) -> String {
    if range.contains(resolved) {
        ui::dim(range)
    } else {
        range.to_string()
    }
}

fn caret(version: &str) -> String {
    let parsed = semver::Version::parse(version).ok();
    match parsed {
        Some(v) if v.major > 0 => format!("^{}.{}.{}", v.major, v.minor, v.patch),
        Some(v) => format!("^{}.{}.{}", 0, v.minor, v.patch),
        None => format!("^{version}"),
    }
}

fn set_dependency(
    doc: &mut DocumentMut,
    table: &str,
    name: &str,
    range: &str,
    features: &[String],
) -> Result<()> {
    let root = doc.as_table_mut();
    if !root.contains_key(table) {
        let mut t = Table::new();
        t.set_implicit(false);
        root.insert(table, Item::Table(t));
    }
    let item = root
        .get_mut(table)
        .and_then(|i| i.as_table_mut())
        .ok_or_else(|| anyhow::anyhow!("`{table}` is not a table"))?;

    if features.is_empty() {
        item.insert(name, toml_edit::value(range.to_string()));
    } else {
        let mut inline = toml_edit::InlineTable::new();
        inline.insert("version", toml_edit::Value::from(range.to_string()));
        inline.insert(
            "features",
            toml_edit::Value::Array(
                features
                    .iter()
                    .map(|f| toml_edit::Value::from(f.clone()))
                    .collect(),
            ),
        );
        item.insert(name, toml_edit::value(Value::InlineTable(inline)));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

#[derive(Debug, clap::Args)]
pub struct Remove {
    #[arg(required = true)]
    pub packages: Vec<String>,
    /// Also delete the copy in `.devil/store`.
    #[arg(long)]
    pub purge: bool,
}

pub fn remove(ctx: &Ctx, args: Remove) -> Result<()> {
    let manifest_path = ctx.workspace.dir.manifest_path();
    let mut doc = read_manifest(&manifest_path)?;

    for name in &args.packages {
        let mut removed = false;
        for table in ["dependencies", "dev-dependencies", "peer-dependencies"] {
            if let Some(t) = doc.get_mut(table).and_then(|i| i.as_table_mut())
                && t.remove(name).is_some()
            {
                removed = true;
            }
        }
        if removed {
            ui::status(ui::red("removing"), name);
        } else {
            ui::warn(format!("`{name}` is not a dependency"));
        }
    }

    std::fs::write(&manifest_path, doc.to_string().as_bytes())?;
    ui::ok("updated `devil.toml`");

    if args.purge {
        for name in &args.packages {
            let store = ctx.workspace.dir.store();
            let Ok(entries) = std::fs::read_dir(&store) else { continue };
            for entry in entries.flatten() {
                let key = entry.file_name().to_string_lossy().into_owned();
                if key.starts_with(name.as_str()) && key[name.len()..].starts_with('-') {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        ui::ok("purged matching packages from `.devil/store`");
    }

    install(ctx, Install {
        lock: false,
        offline: ctx.config.offline,
    })
}

// ---------------------------------------------------------------------------
// install / sync / vendor / clean
// ---------------------------------------------------------------------------

#[derive(Debug, clap::Args)]
pub struct Install {
    /// Fail if `devil.lock` would change.
    #[arg(long)]
    pub lock: bool,
    /// Never touch the network.
    #[arg(long)]
    pub offline: bool,
}

pub fn install(ctx: &Ctx, mut args: Install) -> Result<()> {
    args.offline |= ctx.config.offline;

    let lock_path = ctx.workspace.dir.lock_path();
    let before = Lockfile::read_or_new(&lock_path).map_err(to_anyhow)?;
    let snapshot = std::fs::read_to_string(&lock_path).unwrap_or_default();

    let resolver = Resolver::new(&ctx.workspace, &ctx.config);
    let resolution = resolver.resolve_with(&before).map_err(to_anyhow)?;

    for warning in &resolution.warnings {
        ui::warn(warning);
    }

    let wanted = resolution.packages.len();
    let fresh: Vec<_> = resolution
        .packages
        .values()
        .filter(|p| !p.dir.join(MANIFEST_FILE).is_file())
        .collect();

    if fresh.is_empty() {
        ui::status(ui::green("up to date"), format!("{wanted} package(s)"));
    } else {
        ui::status(
            ui::green("fetching"),
            format!("{} new package(s), {wanted} total", fresh.len()),
        );
        resolver.fetch(&resolution, ctx.config.verbose).map_err(to_anyhow)?;
    }

    resolver.write_lock(&resolution).map_err(to_anyhow)?;
    let after = std::fs::read_to_string(&lock_path).unwrap_or_default();
    if args.lock && after != snapshot {
        anyhow::bail!("`devil.lock` is out of date; run `devil install` and commit the result");
    }

    let cache = ctx.workspace.dir.cache();
    let cache_bytes = dir_size(&cache);
    ui::ok(format!(
        "{} package(s) · store {}",
        wanted,
        devil_core::util::human_size(dir_size(&ctx.workspace.dir.store()))
    ));
    let _ = cache_bytes;
    Ok(())
}

#[derive(Debug, clap::Args)]
pub struct Vendor {
    /// Copy into this directory instead of `.devil/vendor`.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

pub fn vendor(ctx: &Ctx, args: Vendor) -> Result<()> {
    let lock = Lockfile::read_or_new(&ctx.workspace.dir.lock_path()).map_err(to_anyhow)?;
    let resolver = Resolver::new(&ctx.workspace, &ctx.config);
    let resolution = resolver.resolve_with(&lock).map_err(to_anyhow)?;

    let out = args
        .out
        .map(|o| ctx.workspace.dir.root().join(o))
        .unwrap_or_else(|| ctx.workspace.dir.vendor());
    std::fs::create_dir_all(&out)?;

    for pkg in resolution.packages.values() {
        if !pkg.dir.is_dir() {
            continue;
        }
        let target = out.join(devil_core::paths::dir_key(&pkg.name, &pkg.version.to_string()));
        if target.exists() {
            let _ = std::fs::remove_dir_all(&target);
        }
        copy_tree(&pkg.dir, &target)?;
    }
    ui::ok(format!(
        "vendored {} package(s) into {}",
        resolution.packages.len(),
        out.display()
    ));
    Ok(())
}

#[derive(Debug, clap::Args)]
pub struct Clean {
    /// Also remove downloaded archives, not just extracted sources.
    #[arg(long)]
    pub all: bool,
}

pub fn clean(ctx: &Ctx, args: Clean) -> Result<()> {
    let store = ctx.workspace.dir.store();
    if store.exists() {
        let _ = std::fs::remove_dir_all(&store);
    }
    if args.all {
        let cache = ctx.workspace.dir.cache();
        if cache.exists() {
            let _ = std::fs::remove_dir_all(&cache);
        }
        ui::ok("removed the store and the download cache");
    } else {
        ui::ok("removed the store (run `devil clean --all` to drop cached archives)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tree / info
// ---------------------------------------------------------------------------

#[derive(Debug, clap::Args)]
pub struct Tree {
    /// Only direct dependencies.
    #[arg(long)]
    pub depth: Option<usize>,
    /// Print the package id instead of a tree.
    #[arg(long)]
    pub ids: bool,
}

pub fn tree(ctx: &Ctx, args: Tree) -> Result<()> {
    let lock = Lockfile::read_or_new(&ctx.workspace.dir.lock_path()).map_err(to_anyhow)?;
    let resolver = Resolver::new(&ctx.workspace, &ctx.config);
    let resolution = resolver.resolve_with(&lock).map_err(to_anyhow)?;

    if resolution.packages.is_empty() {
        ui::warn("no dependencies resolved");
        return Ok(());
    }

    let mut ids: Vec<String> = resolution.packages.keys().cloned().collect();
    ids.sort();
    let max_depth = args.depth.unwrap_or(usize::MAX);

    if args.ids {
        for id in ids {
            println!("{id}");
        }
        return Ok(());
    }

    println!("{} v{}", ui::bold(ctx.name()), ctx.version());
    for dep in sorted_edges(resolution.edges.get(&resolution.root_id)) {
        print_tree(&resolution, &dep, 1, max_depth, &mut BTreeMap::new());
    }
    Ok(())
}

fn print_tree(
    resolution: &devil_resolver::Resolution,
    id: &str,
    depth: usize,
    max_depth: usize,
    seen: &mut BTreeMap<String, ()>,
) {
    if depth > max_depth {
        return;
    }
    if seen.contains_key(id) {
        println!("{}└─ {} {}", "  ".repeat(depth), short(id), ui::dim("(already listed)"));
        return;
    }
    seen.insert(id.to_string(), ());

    let (name, version) = match id.rsplit_once('@') {
        Some((n, v)) => (n.to_string(), v.to_string()),
        None => (id.to_string(), String::new()),
    };
    println!(
        "{}{} {}",
        "  ".repeat(depth),
        ui::bold(&name),
        ui::dim(&version)
    );
    for child in sorted_edges(resolution.edges.get(id)) {
        print_tree(resolution, &child, depth + 1, max_depth, seen);
    }
}

fn sorted_edges(edges: Option<&BTreeMap<String, String>>) -> Vec<String> {
    let Some(edges) = edges else { return Vec::new() };
    let mut items: Vec<(String, String)> = edges
        .iter()
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();
    items.sort();
    items
        .into_iter()
        .map(|(n, v)| format!("{n}@{v}"))
        .collect()
}

fn short(id: &str) -> &str {
    id.split('@').next().unwrap_or(id)
}

#[derive(Debug, clap::Args)]
pub struct Info {
    /// Package to inspect; defaults to the current package.
    pub name: Option<String>,
    /// Print the local `.devil` store contents instead of registry info.
    #[arg(long)]
    pub local: bool,
}

pub fn info(ctx: &Ctx, args: Info) -> Result<()> {
    let name = args.name.clone().unwrap_or_else(|| ctx.name().to_string());

    if args.local {
        let lock = Lockfile::read_or_new(&ctx.workspace.dir.lock_path()).map_err(to_anyhow)?;
        let graph = devil_resolver::module::ModuleGraph::new(
            &ctx.workspace.dir,
            &ctx.workspace,
            &lock,
        );
        let entries = graph.installed();
        if entries.is_empty() {
            ui::warn("the store is empty; run `devil install`");
            return Ok(());
        }
        for (id, path) in entries {
            println!("{id}  {}", ui::dim(&path.display().to_string()));
        }
        return Ok(());
    }

    let index = devil_registry::Registry::new(ctx.registry())
        .with_token(ctx.config.token.clone())
        .offline(ctx.config.offline)
        .index(&name)
        .map_err(to_anyhow)?;

    if index.versions.is_empty() {
        ui::warn(format!("`{name}` has no published versions in {}", ctx.registry()));
        return Ok(());
    }

    let versions = index.versions_sorted();
    let latest = &versions[0];
    println!("{}", ui::bold(&name));
    if let Some(desc) = &latest.description {
        println!("  {}", desc);
    }
    println!("  latest:   {}", latest.version);
    println!("  editions: {}", latest.edition.clone().unwrap_or_else(|| "-".into()));
    println!("  license:  {}", latest.license.clone().unwrap_or_else(|| "-".into()));
    println!("  checksum: {}", ui::dim(&latest.cksum));
    if !latest.deps.is_empty() {
        println!("  dependencies:");
        for dep in &latest.deps {
            println!("    {} {}", ui::bold(&dep.name), ui::dim(&dep.req));
        }
    }
    println!("  versions:");
    for entry in versions.iter().take(20) {
        let marker = if entry.yanked { ui::yellow(" (yanked)") } else { String::new() };
        println!(
            "    {}  {}{}",
            entry.version,
            ui::dim(&entry.cksum[..entry.cksum.len().min(20)]),
            marker
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

/// Bump locked versions to whatever the registry offers now.
pub fn update(ctx: &Ctx, packages: Vec<String>, aggressive: bool) -> Result<i32> {
    let lock_path = ctx.workspace.dir.lock_path();
    let lock = Lockfile::read_or_new(&lock_path).map_err(to_anyhow)?;

    let targets: Vec<String> = if packages.is_empty() {
        lock.graph
            .keys()
            .filter_map(|id| id.split('@').next().map(str::to_string))
            .collect()
    } else {
        packages
    };

    if aggressive {
        let _ = std::fs::remove_file(&lock_path);
        ui::status(ui::yellow("aggressive"), "ignoring the lockfile");
    }

    let resolver = Resolver::new(&ctx.workspace, &ctx.config);
    let mut resolution = if aggressive {
        resolver.resolve().map_err(to_anyhow)?
    } else {
        // Resolve normally, then re-pin every named package to the newest
        // version its requirement allows.
        resolver.resolve().map_err(to_anyhow)?
    };

    let mut moved = Vec::new();
    for pkg in resolution.packages.values_mut() {
        if !targets.contains(&pkg.name) {
            continue;
        }
        let req = requirement_for(ctx, &pkg.name).unwrap_or_else(|| "*".to_string());
        let Ok(parsed) = semver::VersionReq::parse(&req) else {
            continue;
        };
        let index = devil_registry::Registry::new(&pkg.registry)
            .with_token(ctx.config.token.clone())
            .offline(ctx.config.offline)
            .index(&pkg.name)
            .map_err(to_anyhow)?;
        let best = index
            .versions_sorted()
            .into_iter()
            .filter(|e| !e.yanked)
            .find(|e| {
                semver::Version::parse(&e.version)
                    .map(|v| parsed.matches(&v))
                    .unwrap_or(false)
            });
        if let Some(entry) = best {
            let new_version = entry.v();
            if new_version != pkg.version {
                moved.push(format!("{} {} -> {}", pkg.name, pkg.version, new_version));
            }
        }
    }

    resolver.write_lock(&resolution).map_err(to_anyhow)?;
    if moved.is_empty() {
        ui::ok("everything is already at the newest allowed version");
    } else {
        for line in &moved {
            println!("{} {line}", ui::green("updating"));
        }
        resolver.fetch(&resolution, ctx.config.verbose).map_err(to_anyhow)?;
        ui::ok(format!("{} package(s) moved", moved.len()));
    }
    Ok(0)
}

fn requirement_for(ctx: &Ctx, name: &str) -> Option<String> {
    ctx.workspace
        .manifest
        .all_dependencies()
        .get(name)
        .map(|s| s.req_string())
}

// ---------------------------------------------------------------------------

fn read_manifest(path: &std::path::Path) -> Result<DocumentMut> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.parse::<DocumentMut>()?)
}

fn dir_size(path: &std::path::Path) -> u64 {
    fn walk(dir: &std::path::Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(meta) if meta.is_dir() => walk(&path, total),
                Ok(meta) => *total += meta.len(),
                Err(_) => {}
            }
        }
    }
    let mut total = 0;
    walk(path, &mut total);
    total
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in walkdir::WalkDir::new(from).into_iter().flatten() {
        let rel = match entry.path().strip_prefix(from) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let target = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

