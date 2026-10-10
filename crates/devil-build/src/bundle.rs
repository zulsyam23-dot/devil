//! The bundler: walk the module graph, compile every file, emit one artifact.
//!
//! Output is a tiny CommonJS-style registry, so a whole dependency tree
//! collapses into a single `.js` file with no `node_modules` in sight. Each
//! module keeps its own `require` closure, which means two versions of the same
//! package can coexist inside one bundle.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use swc_core::common::sync::Lrc;
use swc_core::common::SourceMap;
use swc_core::ecma::ast::EsVersion;

use devil_core::lock::Lockfile;
use devil_core::paths::Workspace;
use devil_core::Result;
use devil_resolver::module::{ModuleGraph, Target};

use crate::runtime;
use crate::transpile::{self, CompileOptions};

/// Output shape of the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// `module.exports = …` — libraries consumed by CommonJS loaders.
    #[default]
    Cjs,
    /// `var name = (function(){ … })()` — drop-in browser script.
    Iife,
    /// Native ES modules, one file per module inside `dist/`.
    Esm,
    /// Run the entry for its side effects and export nothing.
    App,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "cjs" | "commonjs" => Some(Format::Cjs),
            "iife" | "browser" => Some(Format::Iife),
            "esm" | "module" => Some(Format::Esm),
            "app" | "bin" => Some(Format::App),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Esm => "mjs",
            _ => "js",
        }
    }
}

/// Where the bundle is meant to run; controls the default externals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Platform {
    /// No platform assumptions; runtime builtins stay external.
    #[default]
    Neutral,
    /// Assume a Node-like host that provides the builtins.
    Node,
    /// Assume a browser: everything is bundled, no `require` calls escape.
    Browser,
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub format: Format,
    pub platform: Platform,
    pub minify: bool,
    pub sourcemap: bool,
    pub target: EsVersion,
    /// Extra package names to leave as bare imports.
    pub externals: Vec<String>,
    /// Global name used by `Format::Iife`.
    pub global_name: Option<String>,
    /// Do not write anything to disk, only build in memory.
    pub dry_run: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            format: Format::Cjs,
            platform: Platform::Neutral,
            minify: false,
            sourcemap: false,
            target: EsVersion::EsNext,
            externals: Vec::new(),
            global_name: None,
            dry_run: false,
        }
    }
}

/// One module in the finished bundle.
#[derive(Debug, Clone)]
pub struct ModuleRecord {
    pub id: String,
    pub path: PathBuf,
    pub code: String,
    /// specifier as written in the source -> resolved module id.
    pub deps: BTreeMap<String, String>,
    /// Builtins this module pulls in.
    pub builtins: BTreeSet<String>,
    pub is_json: bool,
}

#[derive(Debug, Default)]
pub struct Bundle {
    pub modules: Vec<ModuleRecord>,
    pub code: String,
    pub warnings: Vec<String>,
    pub externals: BTreeSet<String>,
}

impl Bundle {
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Total size of the emitted JavaScript.
    pub fn size(&self) -> usize {
        self.code.len()
    }

    pub fn find(&self, id: &str) -> Option<&ModuleRecord> {
        self.modules.iter().find(|m| m.id == id)
    }
}

/// Build a bundle starting at `entry`.
pub fn build(
    workspace: &Workspace,
    lock: &Lockfile,
    entry: &Path,
    opts: &BuildOptions,
) -> Result<Bundle> {
    let mut graph = ModuleGraph::new(&workspace.dir, workspace, lock);
    graph.externals = opts.externals.clone();

    let cm: Lrc<SourceMap> = Default::default();
    let root = workspace.dir.root().to_path_buf();
    let entry = normalise(entry);

    let mut bundle = Bundle::default();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(entry.clone());

    while let Some(file) = queue.pop_front() {
        let id = devil_resolver::module::module_id(&root, &file);
        if seen.contains_key(&id) {
            continue;
        }

        let source = std::fs::read_to_string(&file)
            .map_err(|e| devil_core::error::Error::io(&file, e))?;

        // Resolve every specifier before compiling so a broken import fails with
        // a good message instead of an obscure codegen error.
        let is_json = file.extension().and_then(|e| e.to_str()) == Some("json");
        let mut deps: BTreeMap<String, String> = BTreeMap::new();
        let mut builtins = BTreeSet::new();

        let specs = if is_json {
            Vec::new()
        } else {
            let parsed = parse_only(&cm, &file, &source)?;
            transpile::collect_specifiers(&parsed)
        };

        for spec in specs {
            match graph.resolve(&file, &spec)? {
                Target::Builtin(name) => {
                    builtins.insert(name);
                }
                Target::External(name) => {
                    bundle.externals.insert(name);
                }
                Target::File(target) => {
                    let target_id = devil_resolver::module::module_id(&root, &target);
                    if !seen.contains_key(&target_id) {
                        queue.push_back(target);
                    }
                    deps.insert(spec, target_id);
                }
            }
        }

        let code = if is_json {
            // JSON modules become a CommonJS module wrapping the literal.
            format!("module.exports = {source};")
        } else {
            let rewrite = if opts.format == Format::Esm {
                Some(esm_rewrite_map(&root, &file, &deps))
            } else {
                None
            };
            let compiled = transpile::compile_file(
                &cm,
                &file,
                &source,
                opts.target,
                CompileOptions {
                    commonjs: opts.format != Format::Esm,
                    minify: opts.minify,
                    rewrite,
                },
            )?;
            compiled.code
        };

        seen.insert(id.clone(), bundle.modules.len());
        bundle.modules.push(ModuleRecord {
            id,
            path: file,
            code,
            deps,
            builtins,
            is_json,
        });
    }

    // Stable order: entry first, then discovery order.
    bundle.modules.sort_by(|a, b| {
        let ea = if a.path == entry { 0 } else { 1 };
        let eb = if b.path == entry { 0 } else { 1 };
        ea.cmp(&eb).then(a.id.cmp(&b.id))
    });

    let entry_id = devil_resolver::module::module_id(&root, &entry);
    bundle.code = match opts.format {
        Format::Esm => emit_esm(&bundle, &entry_id),
        other => emit_single(&bundle, &entry_id, other, opts),
    };

    Ok(bundle)
}

/// Parse without transforming, purely to read the specifier list.
fn parse_only(cm: &Lrc<SourceMap>, path: &Path, source: &str) -> Result<swc_core::ecma::ast::Module> {
    let globals = swc_core::common::Globals::new();
    swc_core::common::GLOBALS.set(&globals, || {
        let fm = cm.new_source_file(
            swc_core::common::FileName::Custom(path.to_string_lossy().into_owned()).into(),
            source.to_string(),
        );
        swc_core::ecma::parser::parse_file_as_module(
            &fm,
            transpile::detect_syntax(path),
            EsVersion::EsNext,
            None,
            &mut vec![],
        )
        .map_err(|err| {
            devil_core::error::Error::custom(format!(
                "{}: {}",
                path.display(),
                err.into_kind().msg()
            ))
        })
    })
}

/// Specifier map used for ESM output: every import becomes a relative path.
fn esm_rewrite_map(
    root: &Path,
    from: &Path,
    deps: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let from_id = devil_resolver::module::module_id(root, from);
    for (spec, target_id) in deps {
        let rel = relative_id(&from_id, target_id);
        map.insert(spec.clone(), rel);
    }
    map
}

fn relative_id(from_id: &str, target_id: &str) -> String {
    let from_dir = Path::new(from_id).parent().map(Path::to_path_buf);
    let target = Path::new(target_id);
    match from_dir {
        Some(dir) => {
            let joined = dir.join(target);
            normalise(&joined)
                .to_string_lossy()
                .replace('\\', "/")
        }
        None => target.to_string_lossy().replace('\\', "/"),
    }
}

/// Emit a single-file bundle (CJS, IIFE or app).
fn emit_single(bundle: &Bundle, entry_id: &str, format: Format, opts: &BuildOptions) -> String {
    let mut out = String::new();
    out.push_str(&runtime::header(
        &bundle_name_hint(bundle, entry_id),
        bundle.modules.len(),
    ));
    out.push_str(&runtime::preamble());

    for m in &bundle.modules {
        out.push_str(&runtime::define(
            &m.id,
            &m.code,
            &m.deps,
            &m.builtins,
            &bundle.externals,
        ));
    }

    let call = format!("__devil_require({});", js_string(entry_id));
    match format {
        Format::Cjs => {
            out.push_str(&format!("module.exports = {call}\n"));
        }
        Format::App => {
            out.push_str(&format!("{call}\n"));
        }
        Format::Iife => {
            let name = opts
                .global_name
                .clone()
                .unwrap_or_else(|| "DevilBundle".to_string());
            out.push_str(&format!("var {name} = (function () {{\n"));
            out.push_str(&runtime::preamble());
            for m in &bundle.modules {
                out.push_str(&runtime::define(
                    &m.id,
                    &m.code,
                    &m.deps,
                    &m.builtins,
                    &bundle.externals,
                ));
            }
            out.push_str(&format!("  return __devil_require({});\n", js_string(entry_id)));
            out.push_str("})();\n");
        }
        Format::Esm => unreachable!("ESM is emitted separately"),
    }
    out
}

/// Emit native ES modules, one file per module.
fn emit_esm(bundle: &Bundle, entry_id: &str) -> String {
    // For ESM the "code" field carries the index manifest: consumers use
    // `Bundle::write_esm` to emit the individual files.
    let mut out = String::new();
    for m in &bundle.modules {
        out.push_str(&format!("export * from {};\n", js_string(&m.id)));
        if m.id == entry_id {
            out.push_str(&format!("export {{ default }} from {};\n", js_string(&m.id)));
        }
    }
    out
}

/// Files produced by an ESM build: `(relative path, contents)`.
pub fn esm_files(bundle: &Bundle, entry_id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for m in &bundle.modules {
        let path = format!("{}.mjs", m.id);
        out.push((path, m.code.clone()));
    }
    let index = format!(
        "export * from {spec};\nexport {{ default }} from {spec};\n",
        spec = js_string(&format!("./{entry_id}.mjs"))
    );
    out.insert(0, ("index.mjs".to_string(), index));
    out
}

fn bundle_name_hint(bundle: &Bundle, entry_id: &str) -> String {
    let _ = bundle;
    entry_id.to_string()
}

pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn normalise(path: &Path) -> PathBuf {
    devil_resolver::module::normalise(path)
}

/// Serialise a dependency table into JS source.
pub fn deps_object(deps: &BTreeMap<String, String>) -> String {
    let mut out = String::from("{");
    for (spec, target) in deps {
        out.push_str(&format!("{}:{},", js_string(spec), js_string(target)));
    }
    out.push('}');
    out
}