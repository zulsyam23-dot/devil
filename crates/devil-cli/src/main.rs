//! `devil` — a package manager and bundler for TypeScript, written in Rust.
//!
//! It works like Cargo (a manifest, a lockfile, a content store, deterministic
//! archives) and installs packages as `.devil` files, but the packages it
//! resolves are TypeScript libraries and the toolchain is pure Rust: no Node.js,
//! no `npm`, and no `node_modules` directory anywhere.

mod cmd;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};

use cmd::Ctx;

#[derive(Parser)]
#[command(
    name = "devil",
    version,
    about = "A Cargo-style package manager for TypeScript, with no Node.js required",
    long_about = None,
    propagate_version = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new package in the current directory.
    Init(cmd::init::Args),

    /// Build the package into `dist/`.
    Build(cmd::build::Args),

    /// Run a TypeScript file on the embedded QuickJS runtime.
    Run(cmd::run::Args),

    /// Compile every source file and report problems.
    Check(cmd::registry::Check),

    /// Run every `*.test.ts` file on the embedded runtime.
    Test(cmd::test::Args),

    /// Add one or more dependencies.
    Add(cmd::deps::Add),

    /// Remove dependencies from `devil.toml`.
    #[command(alias = "rm")]
    Remove(cmd::deps::Remove),

    /// Update the lockfile, optionally for a single package.
    Update(Update),

    /// Resolve and download every dependency into `.devil/store`.
    #[command(alias = "i")]
    Install(cmd::deps::Install),

    /// Alias for `install`, for CI pipelines.
    Sync(cmd::deps::Install),

    /// Copy dependencies into a plain directory tree.
    Vendor(cmd::deps::Vendor),

    /// Delete extracted dependencies from the store.
    Clean(cmd::deps::Clean),

    /// Show the resolved dependency tree.
    #[command(alias = "ls")]
    Tree(cmd::deps::Tree),

    /// Show metadata for a package.
    Info(cmd::deps::Info),

    /// Build a `.devil` archive without publishing it.
    Pack(cmd::package::Pack),

    /// Upload a new version to the registry.
    Publish(cmd::package::Publish),

    /// Mark a published version as broken.
    Yank(cmd::package::Yank),

    /// Search the registry.
    #[command(alias = "find")]
    Search(cmd::registry::Search),

    /// Serve a local, read-only registry from a directory.
    #[command(subcommand)]
    Registry(RegistryCommand),

    /// Store or clear the registry API token.
    Login(cmd::registry::Login),

    /// Print `devil.toml` or `devil.lock`.
    Project(cmd::project::Project),
}

#[derive(Debug, clap::Args)]
struct Update {
    /// Only update these packages; defaults to everything.
    pub packages: Vec<String>,
    /// Ignore the current lockfile entirely.
    #[arg(long)]
    pub aggressive: bool,
}

#[derive(Subcommand)]
enum RegistryCommand {
    /// Run a static registry server over HTTP.
    Serve(cmd::registry_server::Serve),
    /// Write `index/` and `dl/` entries from `.devil` archives.
    Index(cmd::registry_server::Index),
}

fn main() {
    let cli = Cli::parse();
    let code = match dispatch(cli.command) {
        Ok(code) => code,
        Err(err) => {
            ui::error(err);
            1
        }
    };
    std::process::exit(code);
}

fn dispatch(command: Command) -> Result<i32> {
    // Commands that do not need a manifest.
    match command {
        Command::Init(args) => return cmd::init::run(args).map(|_| 0),
        Command::Login(args) => return cmd::registry::login(args).map(|_| 0),
        Command::Registry(RegistryCommand::Serve(args)) => {
            return cmd::registry_server::serve(args).map(|_| 0);
        }
        Command::Registry(RegistryCommand::Index(args)) => {
            return cmd::registry_server::index(args).map(|_| 0);
        }
        _ => {}
    }

    let ctx = Ctx::open()?;

    let code: i32 = match command {
        Command::Init(_) | Command::Login(_) => unreachable!("handled above"),

        Command::Build(args) => cmd::build::run(&ctx, args).map(|_| 0)?,
        Command::Check(args) => cmd::registry::check(&ctx, args)?,
        Command::Run(args) => cmd::run::run(&ctx, args)?,
        Command::Test(args) => cmd::test::run(&ctx, args)?,

        Command::Add(args) => cmd::deps::add(&ctx, args).map(|_| 0)?,
        Command::Remove(args) => cmd::deps::remove(&ctx, args).map(|_| 0)?,
        Command::Update(args) => cmd::deps::update(&ctx, args.packages, args.aggressive)?,
        Command::Install(args) | Command::Sync(args) => {
            cmd::deps::install(&ctx, args).map(|_| 0)?
        }
        Command::Vendor(args) => cmd::deps::vendor(&ctx, args).map(|_| 0)?,
        Command::Clean(args) => cmd::deps::clean(&ctx, args).map(|_| 0)?,
        Command::Tree(args) => cmd::deps::tree(&ctx, args).map(|_| 0)?,
        Command::Info(args) => cmd::deps::info(&ctx, args).map(|_| 0)?,

        Command::Pack(args) => cmd::package::pack(&ctx, args).map(|_| 0)?,
        Command::Publish(args) => cmd::package::publish(&ctx, args).map(|_| 0)?,
        Command::Yank(args) => cmd::package::yank(&ctx, args).map(|_| 0)?,

        Command::Search(args) => cmd::registry::search(&ctx, args).map(|_| 0)?,
        Command::Registry(_) => unreachable!("handled above"),
        Command::Project(args) => cmd::project::run(&ctx, args).map(|_| 0)?,
    };

    Ok(code)
}