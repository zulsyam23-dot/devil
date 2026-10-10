//! Registry-facing commands: `search`, `login`, `check` (diagnostics).

use anyhow::Result;

use super::build::to_anyhow;
use super::Ctx;
use crate::ui;
use devil_core::config::Config;
use devil_core::lock::Lockfile;
use devil_registry::Registry;

#[derive(Debug, clap::Args)]
pub struct Search {
    pub query: String,
    #[arg(long, default_value_t = 15)]
    pub limit: usize,
}

pub fn search(ctx: &Ctx, args: Search) -> Result<()> {
    let client = Registry::new(ctx.registry())
        .with_token(ctx.config.token.clone())
        .offline(ctx.config.offline);
    let response = client
        .search(&args.query, args.limit)
        .map_err(to_anyhow)?;

    if response.results.is_empty() {
        ui::warn(format!("no packages match `{}`", args.query));
        return Ok(());
    }
    for hit in &response.results {
        let description = hit
            .description
            .clone()
            .unwrap_or_default();
        println!(
            "{:<28} {:<10} {}",
            ui::bold(&hit.name),
            ui::dim(&hit.version),
            description
        );
    }
    Ok(())
}

#[derive(Debug, clap::Args)]
pub struct Login {
    /// Token to store; omit to read from `DEVIL_TOKEN`.
    #[arg(long)]
    pub token: Option<String>,
    /// Remove the stored token.
    #[arg(long)]
    pub logout: bool,
}

pub fn login(args: Login) -> Result<()> {
    let path = Config::credentials_path();
    let mut config = Config::load()?;

    if args.logout {
        config.token = None;
        config.save()?;
        ui::ok(format!("removed the stored token in {}", path.display()));
        return Ok(());
    }

    let token = match args.token.or_else(|| std::env::var("DEVIL_TOKEN").ok()) {
        Some(t) if !t.is_empty() => t,
        _ => {
            anyhow::bail!(
                "no token supplied\n  hint: devil login --token <token>\n         or export DEVIL_TOKEN=<token>"
            )
        }
    };
    if !token.starts_with("dk_") {
        ui::warn("tokens usually start with `dk_`; storing it verbatim");
    }

    config.token = Some(token);
    config.save()?;
    ui::ok(format!("token saved to {}", path.display()));
    Ok(())
}

#[derive(Debug, clap::Args)]
pub struct Check {
    /// Check dependencies of the whole store, not just this package.
    #[arg(long)]
    pub all: bool,
}

/// Parse every source file the package ships, reporting syntax errors.
pub fn check(ctx: &Ctx, _args: Check) -> Result<i32> {
    use swc_core::common::sync::Lrc;
    use swc_core::common::SourceMap;
    use swc_core::ecma::ast::EsVersion;

    let cm: Lrc<SourceMap> = Default::default();
    let root = ctx.workspace.dir.root();
    let mut checked = 0usize;
    let mut failures = 0usize;

    let entry = ctx.workspace.entry_path();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if entry.is_file() {
        collect_sources(&entry, &mut files);
    }
    for dir in ["src", "tests"] {
        let path = root.join(dir);
        if path.is_dir() {
            collect_sources_dir(&path, &mut files);
        }
    }
    files.sort();
    files.dedup();

    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        if file.extension().and_then(|e| e.to_str()) != Some("json") {
            let result = devil_build::compile_file(
                &cm,
                file,
                &source,
                EsVersion::EsNext,
                devil_build::CompileOptions {
                    commonjs: true,
                    ..Default::default()
                },
            );
            match result {
                Ok(_) => checked += 1,
                Err(e) => {
                    failures += 1;
                    eprintln!("{} {e}", ui::red("error:"));
                }
            }
        } else {
            checked += 1;
        }
    }

    // Dependency state.
    let lock = Lockfile::read_or_new(&ctx.workspace.dir.lock_path()).map_err(to_anyhow)?;
    let resolver = devil_resolver::Resolver::new(&ctx.workspace, &ctx.config);
    let resolution = resolver.resolve_with(&lock).map_err(to_anyhow)?;
    let missing: Vec<String> = resolution
        .packages
        .values()
        .filter(|p| !p.dir.join(devil_core::paths::MANIFEST_FILE).is_file())
        .map(|p| p.id.clone())
        .collect();

    if !missing.is_empty() {
        failures += missing.len();
        for id in &missing {
            eprintln!("{} {id} is not installed", ui::yellow("warning:"));
        }
    }

    println!(
        "{} checked {checked} file(s), {} package(s) resolved",
        if failures == 0 { ui::green("ok") } else { ui::red("failed") },
        resolution.packages.len()
    );
    if failures > 0 {
        return Ok(1);
    }
    Ok(0)
}

fn collect_sources(entry: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let ext = entry.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext, "ts" | "tsx" | "mts" | "cts" | "js" | "mjs" | "cjs" | "json") {
        out.push(entry.to_path_buf());
    }
    if entry.is_dir() {
        collect_sources_dir(entry, out);
    }
}

fn collect_sources_dir(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            !name.starts_with('.') && name != "node_modules" && name != "dist"
        })
        .flatten()
    {
        if entry.file_type().is_file() {
            collect_sources(entry.path(), out);
        }
    }
}