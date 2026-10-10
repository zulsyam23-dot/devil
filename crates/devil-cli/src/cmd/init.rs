//! `devil init` — scaffold a new package.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::ui;
use devil_core::manifest::{
    BinaryTarget, Edition, Manifest, Package, Publish,
};
use devil_core::paths::{DEFAULT_SRC, MANIFEST_FILE};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Package name; defaults to the directory name.
    pub name: Option<String>,
    /// Create a library (the default).
    #[arg(long, conflicts_with = "bin")]
    pub lib: bool,
    /// Create an executable entry point at `src/main.ts`.
    #[arg(long)]
    pub bin: bool,
    /// Create the manifest only, no source files.
    #[arg(long)]
    pub lib_only: bool,
    /// Do not create a git repository note in the README.
    #[arg(long)]
    pub vcs: bool,
}

pub fn run(args: Args) -> Result<()> {
    let dir = std::env::current_dir()?;
    let name = match args.name {
        Some(n) => n,
        None => dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".to_string()),
    };
    devil_core::util::validate_name(&name)?;

    let manifest_path = dir.join(MANIFEST_FILE);
    if manifest_path.exists() {
        anyhow::bail!("`{MANIFEST_FILE}` already exists in {}", dir.display());
    }

    let is_bin = args.bin;
    let manifest = template(&name, is_bin);
    manifest.write(&manifest_path)?;
    ui::ok(format!("created `{MANIFEST_FILE}`"));

    if !args.lib_only {
        let entry = if is_bin {
            dir.join(DEFAULT_SRC).join("main.ts")
        } else {
            dir.join(DEFAULT_SRC).join("index.ts")
        };
        write_if_missing(&entry, &entry_template(&name, is_bin))?;
        ui::ok(format!(
            "created {}",
            entry.strip_prefix(&dir).unwrap_or(&entry).display()
        ));

        if is_bin {
            let helper = dir.join(DEFAULT_SRC).join("greet.ts");
            write_if_missing(&helper, &greet_template())?;
            ui::ok("created src/greet.ts");
        }
    }

    let readme = dir.join("README.md");
    write_if_missing(&readme, &readme_template(&name))?;
    ui::ok("created `README.md`");

    let gitignore = dir.join(".gitignore");
    write_if_missing(&gitignore, "/dist\n/.devil\n")?;
    ui::ok("created `.gitignore`");

    if !is_bin {
        let test = dir.join(DEFAULT_SRC).join("index.test.ts");
        write_if_missing(&test, &test_template(&name))?;
        ui::ok("created `src/index.test.ts`");
    }

    println!();
    println!("  next steps:");
    println!("    cd .");
    println!("    devil run{}", if is_bin { "" } else { " src/index.test.ts" });
    println!("    devil add <package>");
    Ok(())
}

fn template(name: &str, is_bin: bool) -> Manifest {
    Manifest {
        package: Package {
            name: name.to_string(),
            version: semver::Version::new(0, 1, 0),
            edition: Edition::LATEST,
            description: Some(format!("A devil package called {name}.")),
            authors: vec![],
            license: Some("MIT".into()),
            repository: None,
            homepage: None,
            documentation: None,
            keywords: vec![],
            categories: vec![],
            readme: Some("README.md".into()),
            entry: Some(if is_bin {
                "src/main.ts".into()
            } else {
                "src/index.ts".into()
            }),
            include: vec!["src".into(), "README.md".into()],
            exclude: vec![],
            publish: Some(Publish {
                enabled: true,
                registry: None,
            }),
        },
        dependencies: Default::default(),
        dev_dependencies: Default::default(),
        peer_dependencies: Default::default(),
        features: Default::default(),
        registries: Default::default(),
        bin: if is_bin {
            vec![BinaryTarget {
                name: name.to_string(),
                path: "src/main.ts".into(),
            }]
        } else {
            vec![]
        },
        profile: Default::default(),
    }
}

fn entry_template(name: &str, is_bin: bool) -> String {
    if is_bin {
        format!(
            "// {name}\n\
             //\n\
             // `devil run` executes this file on the embedded QuickJS runtime.\n\
             // No Node.js required.\n\
             \n\
             import {{ greet }} from \"./greet.js\";\n\
             \n\
             console.log(greet(\"{name}\"));\n"
        )
    } else {
        format!(
            "// {name}\n\
             //\n\
             // Everything exported here becomes the public surface of the package.\n\
             \n\
             export interface Greeting {{\n\
             \x20 name: string;\n\
             }}\n\
             \n\
             export function greet(who: string): string {{\n\
             \x20 return `hello, ${{who}}`;\n\
             }}\n\
             \n\
             export const VERSION = \"0.1.0\";\n"
        )
    }
}

fn greet_template() -> String {
    "// `node:path` is served by the embedded runtime, not by a package.\n\
     import * as path from \"node:path\";\n\
     \n\
     /// Demonstrates a relative import plus a runtime builtin.\n\
     export function greet(who: string): string {\n\
     \x20 const label = path.basename(String(who)) || \"world\";\n\
     \x20 return `hello, ${label}`;\n\
     }\n"
        .to_string()
}

fn test_template(name: &str) -> String {
    format!(
        "// Tiny test harness: no runner to install, `devil test` drives it.\n\
         import {{ greet }} from \"./index.js\";\n\
         \n\
         test(\"greet greets\", () => {{\n\
         \x20 assertEquals(greet(\"world\"), \"hello, world\");\n\
         }});\n\
         \n\
         void \"{name}\";\n"
    )
}

fn readme_template(name: &str) -> String {
    format!(
        "# {name}\n\
         \n\
         A TypeScript package managed with [devil](https://github.com/devil-lang/devil).\n\
         \n\
         ## Layout\n\
         \n\
         ```text\n\
         src/          your TypeScript sources\n\
         dist/         build output (created by `devil build`)\n\
         .devil/       dependency store — this replaces node_modules\n\
         devil.toml    the manifest\n\
         devil.lock    the resolved graph\n\
         ```\n\
         \n\
         ## Commands\n\
         \n\
         ```sh\n\
         devil build            # bundle to dist/\n\
         devil test             # run *.test.ts on the embedded runtime\n\
         devil run src/main.ts  # execute without installing Node.js\n\
         devil add <package>    # add a dependency\n\
         ```\n"
    )
}

fn write_if_missing(path: &Path, contents: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let target: PathBuf = path.to_path_buf();
    std::fs::write(&target, contents)?;
    Ok(())
}

