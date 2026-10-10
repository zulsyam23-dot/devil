//! `devil run` — execute a TypeScript file on the embedded QuickJS runtime.

use std::path::PathBuf;

use anyhow::Result;

use super::build::{ensure_installed, to_anyhow};
use super::Ctx;
use crate::ui;
use devil_build::{BuildOptions, Format};
use devil_core::lock::Lockfile;
use devil_resolver::module::resolve_file;
use devil_vm::{Vm, VmOptions};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Entry module; defaults to `package.entry` in `devil.toml`.
    pub file: Option<PathBuf>,
    /// Arguments forwarded to the script (`process.argv.slice(2)`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
    /// Evaluate an expression instead of a file.
    #[arg(short = 'e', long = "eval")]
    pub expr: Option<String>,
    /// Build only, print the bundle instead of running it.
    #[arg(long)]
    pub print: bool,
    /// Compile without executing.
    #[arg(long)]
    pub no_run: bool,
    /// Keep the bundle in memory; do not touch `dist/`.
    #[arg(long)]
    pub no_cache: bool,
    /// Give up on pending timers after this many seconds.
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,
}

pub fn run(ctx: &Ctx, args: Args) -> Result<i32> {
    let lock = Lockfile::read_or_new(&ctx.workspace.dir.lock_path()).map_err(to_anyhow)?;
    ensure_installed(ctx, &lock, &[])?;

    let (entry, prelude) = match (&args.file, &args.expr) {
        (Some(f), _) => {
            let joined = ctx.workspace.dir.root().join(f);
            let entry = resolve_file(&joined).map_err(to_anyhow)?;
            (entry, String::new())
        }
        (None, Some(expr)) => {
            // Evaluate an expression against the package entry, if there is one.
            let entry = ctx.workspace.entry_path();
            let prelude = format!("\n;({expr})\n");
            if entry.is_file() {
                (entry, prelude)
            } else {
                let scratch = ctx.workspace.dir.root().join(".devil").join("eval.ts");
                std::fs::create_dir_all(scratch.parent().unwrap())?;
                std::fs::write(&scratch, "export default 0;\n")?;
                (scratch, prelude)
            }
        }
        (None, None) => {
            let path = ctx.workspace.entry_path();
            if !path.is_file() {
                anyhow::bail!(
                    "no entry to run; pass a file, or set `package.entry` in devil.toml\n  hint: devil run src/main.ts"
                );
            }
            (path, String::new())
        }
    };

    let opts = BuildOptions {
        format: Format::App,
        minify: false,
        dry_run: true,
        ..Default::default()
    };

    let bundle = devil_build::build(&ctx.workspace, &lock, &entry, &opts).map_err(to_anyhow)?;

    if args.print {
        print!("{}", bundle.code);
        return Ok(0);
    }
    if args.no_run {
        ui::ok(format!(
            "compiled {} module(s) from {}",
            bundle.module_count(),
            entry.display()
        ));
        return Ok(0);
    }

    let source = format!("{}\n{}", bundle.code, prelude);

    let vm_opts = VmOptions {
        script_name: entry
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bundle.js".into()),
        args: args.args.clone(),
        cwd: ctx.workspace.dir.root().to_path_buf(),
        max_wait: std::time::Duration::from_secs(args.timeout),
        ..Default::default()
    };

    let vm = Vm::new(&vm_opts).map_err(to_anyhow)?;
    let _ = args.no_cache;
    match vm.run(&source) {
        Ok(outcome) => Ok(outcome.exit_code()),
        Err(e) => {
            eprintln!("{} {e}", ui::red("uncaught exception:"));
            Ok(1)
        }
    }
}

/// Shared helper: build a bundle and run it, used by `devil test` too.
pub fn run_bundle(ctx: &Ctx, bundle_code: &str, name: &str, args: &[String]) -> Result<i32> {
    let vm_opts = VmOptions {
        script_name: name.to_string(),
        args: args.to_vec(),
        cwd: ctx.workspace.dir.root().to_path_buf(),
        ..Default::default()
    };
    let vm = Vm::new(&vm_opts).map_err(to_anyhow)?;
    match vm.run(bundle_code) {
        Ok(outcome) => Ok(outcome.exit_code()),
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}
