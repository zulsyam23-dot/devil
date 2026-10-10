//! `devil build` — bundle the package into `dist/`.

use std::path::PathBuf;

use anyhow::Result;

use super::Ctx;
use crate::ui;
use devil_build::{BuildOptions, Format, Platform};
use devil_core::lock::Lockfile;
use devil_resolver::module::resolve_file;
use devil_resolver::Resolver;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Entry module; defaults to `package.entry` in `devil.toml`.
    pub entry: Option<PathBuf>,
    /// Output directory; defaults to `dist/`.
    #[arg(long, short = 'o')]
    pub out: Option<PathBuf>,
    /// Output format.
    #[arg(long, value_parser = ["cjs", "iife", "esm", "app", "browser"])]
    pub format: Option<String>,
    /// Target platform assumptions.
    #[arg(long, value_parser = ["neutral", "node", "browser"])]
    pub platform: Option<String>,
    /// Minify the output.
    #[arg(long)]
    pub minify: bool,
    /// Use the `[profile.release]` settings.
    #[arg(long)]
    pub release: bool,
    /// Leave these packages unbundled.
    #[arg(long, value_delimiter = ',')]
    pub external: Vec<String>,
    /// Global variable name for `--format iife`.
    #[arg(long)]
    pub name: Option<String>,
    /// Do not write files, only report.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(ctx: &Ctx, args: Args) -> Result<()> {
    let profile = ctx.workspace.manifest.profile.get(if args.release {
        "release"
    } else {
        "debug"
    });

    let format = args
        .format
        .clone()
        .or_else(|| profile.and_then(|p| p.format.clone()))
        .map(|f| {
            Format::parse(&f)
                .ok_or_else(|| anyhow::anyhow!("unknown format `{f}`; use cjs, iife, esm or app"))
        })
        .transpose()?
        .unwrap_or(Format::Cjs);

    let platform = args
        .platform
        .as_deref()
        .map(|p| match p {
            "node" => Platform::Node,
            "browser" => Platform::Browser,
            _ => Platform::Neutral,
        })
        .unwrap_or_default();

    let minify = args.minify || profile.and_then(|p| p.minify).unwrap_or(args.release);
    let externals: Vec<String> = args
        .external
        .iter()
        .cloned()
        .chain(profile.map(|p| p.external.clone()).unwrap_or_default())
        .collect();

    let entry = match args.entry {
        Some(e) => {
            let joined = ctx.workspace.dir.root().join(e);
            resolve_file(&joined).map_err(to_anyhow)?
        }
        None => {
            let path = ctx.workspace.entry_path();
            if !path.is_file() {
                anyhow::bail!(
                    "entry module not found: {}\n  hint: create it, or set `package.entry` in devil.toml",
                    path.display()
                );
            }
            path
        }
    };

    let lock = Lockfile::read_or_new(&ctx.workspace.dir.lock_path()).map_err(to_anyhow)?;
    ensure_installed(ctx, &lock, &externals)?;

    let opts = BuildOptions {
        format,
        platform,
        minify,
        sourcemap: profile.and_then(|p| p.sourcemap).unwrap_or(false),
        target: target_version(ctx),
        externals,
        global_name: args.name.clone().or_else(|| Some(ctx.name().to_string())),
        dry_run: args.dry_run,
    };

    let bundle = devil_build::build(&ctx.workspace, &lock, &entry, &opts).map_err(to_anyhow)?;

    for warning in &bundle.warnings {
        ui::warn(warning);
    }

    let out_dir = args
        .out
        .map(|o| ctx.workspace.dir.root().join(o))
        .unwrap_or_else(|| ctx.workspace.dir.dist());
    let name = ctx.name().to_string();
    let version = ctx.version();

    let written: Vec<PathBuf> = if args.dry_run {
        Vec::new()
    } else {
        write_output(&out_dir, &name, &bundle, &opts)?
    };

    let total: usize = written.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len() as usize).sum();
    println!(
        "{} {} v{} · {} module(s) · {}",
        ui::green("built"),
        ui::bold(&name),
        version,
        bundle.module_count(),
        if args.dry_run {
            ui::dim("dry run, nothing written").to_string()
        } else {
            devil_core::util::human_size(total as u64)
        }
    );
    for path in &written {
        println!("  {}", ui::dim(&path.display().to_string()));
    }
    if !bundle.externals.is_empty() && matches!(platform, Platform::Browser) {
        ui::warn(format!(
            "left unbundled for the browser: {}",
            bundle.externals.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(())
}

fn write_output(
    out_dir: &PathBuf,
    name: &str,
    bundle: &devil_build::Bundle,
    opts: &BuildOptions,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    let mut written = Vec::new();

    match opts.format {
        Format::Esm => {
            let entry_id = bundle
                .modules
                .first()
                .map(|m| m.id.clone())
                .unwrap_or_default();
            for (rel, contents) in devil_build::bundle::esm_files(bundle, &entry_id) {
                let path = out_dir.join("esm").join(rel);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, contents)?;
                written.push(path);
            }
            let types = out_dir.join("index.d.ts");
            if let Some(src) = declaration_entry(name, opts) {
                let _ = src;
            }
            let _ = types;
        }
        other => {
            let path = out_dir.join(format!("{name}.{}", other.extension()));
            std::fs::write(&path, &bundle.code)?;
            written.push(path);
        }
    }

    if opts.minify {
        let minified = out_dir.join(format!("{name}.min.js"));
        let _ = std::fs::write(&minified, &bundle.code);
    }
    Ok(written)
}

fn declaration_entry(_name: &str, _opts: &BuildOptions) -> Option<PathBuf> {
    None
}

fn target_version(ctx: &Ctx) -> devil_build::EsVersion {
    use devil_build::EsVersion as E;
    match ctx.workspace.manifest.package.edition.es_version_str() {
        "es2015" => E::Es2015,
        "es2017" => E::Es2017,
        "es2018" => E::Es2018,
        "es2019" => E::Es2019,
        "es2020" => E::Es2020,
        "es2021" => E::Es2021,
        "es2023" => E::Es2023,
        _ => E::Es2022,
    }
}

/// Make sure every locked dependency is present in `.devil/store`.
pub fn ensure_installed(ctx: &Ctx, lock: &Lockfile, externals: &[String]) -> Result<()> {
    let resolver = Resolver::new(&ctx.workspace, &ctx.config);
    let resolution = resolver.resolve_with(lock).map_err(to_anyhow)?;
    let needed: Vec<_> = resolution
        .packages
        .values()
        .filter(|p| !externals.contains(&p.name))
        .collect();

    let missing: Vec<_> = needed
        .iter()
        .filter(|p| !p.dir.join(devil_core::paths::MANIFEST_FILE).is_file())
        .collect();

    if missing.is_empty() {
        return Ok(());
    }
    if ctx.config.offline && !missing.is_empty() {
        anyhow::bail!(
            "{} package(s) are missing from the store and --offline is set:\n{}",
            missing.len(),
            missing
                .iter()
                .map(|p| format!("  {} -> {}", p.id, p.dir.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    ui::status(ui::dim("fetching"), format!("{} package(s)", missing.len()));
    resolver.fetch(&resolution, ctx.config.verbose).map_err(to_anyhow)?;
    Ok(())
}

/// Convert a `devil_core` error into `anyhow`, keeping the message intact.
pub fn to_anyhow<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}