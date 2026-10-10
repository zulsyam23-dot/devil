//! `devil pack` and `devil publish`.

use std::path::PathBuf;

use anyhow::Result;
use toml_edit::DocumentMut;

use super::Ctx;
use crate::ui;
use devil_core::util::human_size;
use devil_package::PackInfo;
use devil_registry::{PublishRequest, Registry};

#[derive(Debug, clap::Args)]
pub struct Pack {
    /// Destination `.devil` file; defaults to `<parent>/<name>-<version>.devil`.
    #[arg(long, short = 'o')]
    pub out: Option<PathBuf>,
    /// Only verify, do not write.
    #[arg(long)]
    pub check: bool,
}

pub fn pack(ctx: &Ctx, args: Pack) -> Result<()> {
    let info = pack_now(ctx, args.out.clone(), args.check)?;
    if args.check {
        return Ok(());
    }
    println!("{}", ui::green("packed"));
    println!("  {}", info.path.display());
    println!(
        "  {} file(s), {} packed, {} unpacked",
        info.meta.file_count,
        human_size(info.archive_size),
        human_size(info.meta.unpacked_size)
    );
    println!("  {}", ui::dim(&info.archive_checksum));
    Ok(())
}

pub fn pack_now(ctx: &Ctx, out: Option<PathBuf>, check: bool) -> Result<PackInfo> {
    let root = ctx.workspace.dir.root();
    let manifest = &ctx.workspace.manifest;
    verify_metadata(ctx)?;

    if check {
        let info = devil_package::pack(root, manifest, Some(&std::path::PathBuf::from(
            std::env::temp_dir().join(format!("{}-{}.devil.check", manifest.package.name, manifest.package.version)),
        )))?;
        let _ = std::fs::remove_file(&info.path);
        return Ok(info);
    }
    devil_package::pack(root, manifest, out.as_deref())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[derive(Debug, clap::Args)]
pub struct Publish {
    /// Run every check but do not upload.
    #[arg(long)]
    pub dry_run: bool,
    /// Allow publishing while `devil.lock` is out of date.
    #[arg(long)]
    pub allow_dirty: bool,
    /// Bypass the `package.publish = false` guard.
    #[arg(long)]
    pub force: bool,
    /// Publish to this registry instead of the configured one.
    #[arg(long)]
    pub registry: Option<String>,
    /// Bump the patch/minor/major version before publishing.
    #[arg(long, value_parser = ["major", "minor", "patch"])]
    pub bump: Option<String>,
}

pub fn publish(ctx: &Ctx, args: Publish) -> Result<()> {
    if let Some(level) = args.bump.as_deref() {
        bump_version(ctx, level)?;
    }

    let manifest = &ctx.workspace.manifest;
    let allowed = manifest
        .package
        .publish
        .as_ref()
        .map(|p| p.allowed())
        .unwrap_or(true);
    if !allowed && !args.force {
        anyhow::bail!(
            "`publish = false` in devil.toml\n  hint: remove the flag or pass --force"
        );
    }

    verify_metadata(ctx)?;

    let registry_url = args
        .registry
        .clone()
        .unwrap_or_else(|| ctx.registry());

    let info = pack_now(ctx, Some(std::env::temp_dir().join(format!(
        "{}-{}.devil",
        manifest.package.name, manifest.package.version
    ))), false)?;
    let archive = info.path.clone();

    let request = PublishRequest {
        name: info.meta.name.clone(),
        version: info.meta.version.clone(),
        cksum: info.archive_checksum.clone(),
        size: info.archive_size,
        description: info.meta.description.clone(),
        readme: manifest
            .package
            .readme
            .as_ref()
            .and_then(|r| std::fs::read_to_string(ctx.workspace.dir.root().join(r)).ok()),
        deps: serde_json::to_value(&info.meta.dependencies).unwrap_or_default(),
        yanked: false,
    };

    println!("{}", ui::dim(&format!(
        "{} v{} · {} file(s) · {}",
        request.name,
        request.version,
        info.meta.file_count,
        human_size(info.archive_size)
    )));

    if args.dry_run {
        ui::ok(format!(
            "dry run: {} would be published to {}",
            request.name,
            registry_url
        ));
        let _ = std::fs::remove_file(&archive);
        return Ok(());
    }

    let client = Registry::new(registry_url.clone()).with_token(ctx.config.token.clone());
    if ctx.config.token.is_none() {
        anyhow::bail!(
            "no API token found\n  hint: run `devil login --token <token>` or set DEVIL_TOKEN"
        );
    }

    let response = client
        .publish(&request, &archive)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let _ = std::fs::remove_file(&archive);

    for warning in &response.warnings {
        ui::warn(warning);
    }
    if response.ok {
        ui::ok(format!(
            "published {}@{} to {}",
            request.name,
            request.version,
            registry_url
        ));
    } else {
        anyhow::bail!(
            "registry refused the upload: {}",
            response
                .message
                .unwrap_or_else(|| "no reason given".into())
        );
    }
    Ok(())
}

#[derive(Debug, clap::Args)]
pub struct Yank {
    /// Version to yank.
    pub version: String,
    /// Un-yank instead.
    #[arg(long)]
    pub unyank: bool,
    /// Reason recorded by the registry.
    #[arg(long)]
    pub reason: Option<String>,
}

pub fn yank(ctx: &Ctx, args: Yank) -> Result<()> {
    let name = ctx.name().to_string();
    let request = devil_registry::YankRequest {
        name: name.clone(),
        version: args.version.clone(),
        yanked: !args.unyank,
        reason: args.reason.clone(),
    };
    let client = Registry::new(ctx.registry()).with_token(ctx.config.token.clone());
    client.yank(&request).map_err(|e| anyhow::anyhow!("{e}"))?;
    ui::ok(format!(
        "{} {name}@{}",
        if args.unyank { "unyanked" } else { "yanked" },
        args.version
    ));
    Ok(())
}

fn bump_version(ctx: &Ctx, level: &str) -> Result<()> {
    let path = ctx.workspace.dir.manifest_path();
    let mut doc = std::fs::read_to_string(&path)?
        .parse::<DocumentMut>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let current = ctx.workspace.version();
    let next = match level {
        "major" => semver::Version::new(current.major + 1, 0, 0),
        "minor" => semver::Version::new(current.major, current.minor + 1, 0),
        _ => semver::Version::new(current.major, current.minor, current.patch + 1),
    };
    doc["package"]["version"] = toml_edit::value(next.to_string());
    std::fs::write(&path, doc.to_string().as_bytes())?;
    ui::status(ui::green("bumping"), format!("{current} -> {next}"));
    Ok(())
}

/// Reject the mistakes that make a published package useless.
fn verify_metadata(ctx: &Ctx) -> Result<()> {
    let manifest = &ctx.workspace.manifest;
    let mut problems: Vec<String> = Vec::new();

    if manifest.package.description.is_none() {
        problems.push("`package.description` is missing".into());
    }
    if manifest.package.license.is_none() {
        problems.push("`package.license` is missing".into());
    }
    let has_types = ctx
        .workspace
        .entry_path()
        .exists();
    if !has_types {
        problems.push(format!("entry `{}` does not exist", manifest.package.entry.clone().unwrap_or_default()));
    }
    for target in &manifest.bin {
        let path = ctx.workspace.dir.root().join(&target.path);
        if !path.exists() {
            problems.push(format!("bin target `{}` does not exist", target.path));
        }
    }

    if problems.is_empty() {
        return Ok(());
    }
    let list = problems
        .iter()
        .map(|p| format!("  - {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::bail!("refusing to package an incomplete manifest:\n{list}")
}