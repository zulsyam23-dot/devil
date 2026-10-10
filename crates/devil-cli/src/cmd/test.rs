//! `devil test` — a dependency-free test runner.
//!
//! There is no `node_modules` to install a runner from, so `devil` ships one:
//! it bundles every `*.test.ts` it finds and executes it on the embedded
//! runtime with a tiny global harness injected into the bundle.

use std::path::{Path, PathBuf};

use anyhow::Result;
use walkdir::WalkDir;

use super::build::{ensure_installed, to_anyhow};
use super::Ctx;
use crate::ui;
use devil_build::BuildOptions;
use devil_core::lock::Lockfile;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Only run files whose name contains this string.
    pub filter: Option<String>,
    /// Watch for changes and re-run.
    #[arg(long)]
    pub watch: bool,
    /// Stop after the first failing file.
    #[arg(long)]
    pub bail: bool,
}

/// Prepended to every test bundle. Registers the globals a test file expects.
const HARNESS: &str = r#"
(function () {
  const state = { passed: 0, failed: 0, failures: [], current: "" };
  globalThis.__devil_test_state = state;

  globalThis.test = function (name, fn) {
    state.current = name;
    try {
      fn();
      state.passed++;
    } catch (err) {
      state.failed++;
      state.failures.push({ name: name, error: (err && err.stack) || String(err) });
    }
  };

  const fail = (message) => { throw new Error(message); };
  globalThis.assert = {
    ok: (v, m) => (v ? undefined : fail(m || "expected a truthy value")),
    equal: (a, b, m) => (a == b ? undefined : fail((m ? m + ": " : "") + a + " != " + b)),
    strictEqual: (a, b, m) => (a === b ? undefined : fail((m ? m + ": " : "") + a + " !== " + b)),
    notEqual: (a, b, m) => (a != b ? undefined : fail(m || "values should differ")),
    deepEqual: (a, b, m) =>
      JSON.stringify(a) === JSON.stringify(b) ? undefined : fail(m || "objects differ"),
    throws: (fn, m) => {
      try { fn(); } catch (_) { return; }
      fail(m || "expected the function to throw");
    },
  };
  globalThis.assertEquals = (a, b, m) => globalThis.assert.strictEqual(a, b, m);
  globalThis.assertThrows = (fn, m) => globalThis.assert.throws(fn, m);

  const suites = [];
  globalThis.describe = (name, fn) => { suites.push(name); fn(); suites.pop(); };
  globalThis.it = globalThis.test;
  globalThis.beforeEach = () => {};
  globalThis.afterEach = () => {};
})();
"#;

/// Appended after the bundle so the report is printed once the file finishes.
const REPORTER: &str = r#"
(function () {
  const state = globalThis.__devil_test_state;
  if (!state) return;
  for (const failure of state.failures) {
    __devil_print_err("  FAIL  " + failure.name);
    __devil_print_err("        " + String(failure.error).split("\n")[0]);
  }
  __devil_print("  " + state.passed + " passed, " + state.failed + " failed");
  globalThis.__devil_test_exit = state.failed > 0 ? 1 : 0;
})();
"#;

pub fn run(ctx: &Ctx, args: Args) -> Result<i32> {
    let lock = Lockfile::read_or_new(&ctx.workspace.dir.lock_path()).map_err(to_anyhow)?;
    ensure_installed(ctx, &lock, &[])?;

    let mut files = discover(&ctx.workspace.dir.root(), args.filter.as_deref());
    if files.is_empty() {
        ui::warn("no test files found (looked for `*.test.ts` in `tests/` and `src/`)");
        return Ok(0);
    }

    let mut failed_files;
    loop {
        failed_files = 0;
        for file in &files {
            if execute(ctx, &lock, file).unwrap_or(1) != 0 {
                failed_files += 1;
                if args.bail {
                    break;
                }
            }
        }

        if !args.watch {
            break;
        }
        println!();
        println!("{}", ui::dim("watching for changes, press Ctrl+C to stop"));
        std::thread::sleep(std::time::Duration::from_secs(1));
        files = discover(&ctx.workspace.dir.root(), args.filter.as_deref());
    }

    if failed_files > 0 {
        ui::error(format!(
            "{} of {} test file(s) failed",
            failed_files,
            files.len()
        ));
        return Ok(1);
    }
    ui::ok(format!("{} test file(s) passed", files.len()));
    Ok(0)
}

fn execute(ctx: &Ctx, lock: &Lockfile, file: &PathBuf) -> Result<i32> {
    let rel = file
        .strip_prefix(ctx.workspace.dir.root())
        .unwrap_or(file)
        .display()
        .to_string();

    let opts = BuildOptions {
        format: devil_build::Format::App,
        dry_run: true,
        ..Default::default()
    };
    let bundle = devil_build::build(&ctx.workspace, lock, file, &opts).map_err(to_anyhow)?;
    let source = format!("{HARNESS}\n{}\n{REPORTER}", bundle.code);

    println!("{}", ui::dim(&format!("running {rel}")));
    super::run::run_bundle(ctx, &source, file.to_string_lossy().as_ref(), &[])
        .map_err(|e| {
            eprintln!("{} {rel}: {e}", ui::red("error:"));
            anyhow::anyhow!("{rel}")
        })
}

fn discover(root: &Path, filter: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sub in ["tests", "src", "test"] {
        let dir = root.join(sub);
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                !name.starts_with('.') && name != "node_modules"
            })
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.ends_with(".test.ts") || name.ends_with(".test.tsx") || name.ends_with("_test.ts"))
            {
                continue;
            }
            if let Some(f) = filter
                && !name.contains(f)
            {
                continue;
            }
            out.push(entry.path().to_path_buf());
        }
    }
    out.sort();
    out.dedup();
    out
}