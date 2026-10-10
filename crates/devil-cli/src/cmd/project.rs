//! `devil project` — print or edit the manifest from the command line.

use anyhow::Result;
use toml_edit::DocumentMut;

use super::Ctx;
use crate::ui;

#[derive(Debug, clap::Args)]
pub struct Project {
    /// Show `devil.toml` with comments preserved.
    #[arg(long)]
    pub show: bool,
    /// Print `name@version` only, for scripts.
    #[arg(long)]
    pub id: bool,
    /// Print the resolved lockfile summary.
    #[arg(long)]
    pub lock: bool,
}

pub fn run(ctx: &Ctx, args: Project) -> Result<()> {
    if args.id {
        println!("{}@{}", ctx.name(), ctx.version());
        return Ok(());
    }
    if args.lock {
        let path = ctx.workspace.dir.lock_path();
        if !path.is_file() {
            ui::warn("no devil.lock yet; run `devil install`");
            return Ok(());
        }
        let text = std::fs::read_to_string(path)?;
        let doc = text.parse::<DocumentMut>()?;
        print!("{doc}");
        return Ok(());
    }
    let text = std::fs::read_to_string(ctx.workspace.dir.manifest_path())?;
    print!("{text}");
    Ok(())
}