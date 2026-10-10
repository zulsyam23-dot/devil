//! Turning one TypeScript file into plain JavaScript, using swc.
//!
//! The pipeline is deliberately small and predictable:
//!
//! ```text
//! source ─▶ parse (TsSyntax) ─▶ strip types ─▶ resolve ─▶ commonjs ─▶ emit
//! ```
//!
//! Every pass is explicit so the emitted code stays readable and debuggable,
//! which matters far more here than shaving off the last byte.

use std::path::Path;

use swc_core::common::sync::Lrc;
use swc_core::common::{FileName, Globals, Mark, SourceMap, GLOBALS};
use swc_core::ecma::ast::{
    Callee, CallExpr, EsVersion, Expr, ExprOrSpread, ExportAll, ImportDecl, Invalid, Lit,
    NamedExport, ParenExpr, Program, Str,
};
use swc_core::ecma::codegen::text_writer::JsWriter;
use swc_core::ecma::codegen::{Config as CodegenConfig, Emitter};
use swc_core::ecma::parser::{parse_file_as_module, Syntax, TsSyntax};
use swc_core::ecma::visit::VisitMut;
use swc_core::ecma::transforms::base::helpers::{self as swc_helpers, HELPERS};
use swc_core::ecma::transforms::base::resolver as resolver_pass;
use swc_core::ecma::transforms::module::common_js;
use swc_core::ecma::transforms::module::path::Resolver;
use swc_core::ecma::transforms::typescript::strip;

use devil_core::error::Error;
use devil_core::Result;

/// Compiled output for one source file.
pub struct Compiled {
    pub code: String,
    pub line_count: u32,
    /// `true` for `.d.ts` files, which emit nothing but declarations.
    pub is_declaration_file: bool,
}

impl Compiled {
    pub fn is_empty(&self) -> bool {
        self.code.trim().is_empty()
    }
}

/// Which syntax a source file uses.
pub fn detect_syntax(path: &Path) -> Syntax {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let is_ts = matches!(ext, "ts" | "mts" | "cts" | "tsx");
    if is_ts {
        Syntax::Typescript(TsSyntax {
            tsx: ext == "tsx",
            decorators: false,
            dts: is_dts(path),
            ..Default::default()
        })
    } else {
        Syntax::Es(Default::default())
    }
}

pub fn is_dts(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".d.ts"))
        .unwrap_or(false)
}

pub fn is_typescript(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts") | Some("tsx") | Some("mts") | Some("cts")
    )
}

/// Parse and compile a single file.
pub fn compile_file(
    cm: &Lrc<SourceMap>,
    path: &Path,
    source: &str,
    target: EsVersion,
    opts: CompileOptions,
) -> Result<Compiled> {
    let globals = Globals::new();
    let dts = is_dts(path);
    let typescript = is_typescript(path);

    GLOBALS.set(&globals, || {
        // swc emits its Babel-style helpers (`_interop_require_default`, …)
        // through this scoped TLS slot, and creating the helpers allocates a
        // hygiene mark, so it must happen inside `GLOBALS`.
        let helpers = swc_helpers::Helpers::new(false);
        HELPERS.set(&helpers, || {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();

        let fm = cm.new_source_file(
            FileName::Custom(path.to_string_lossy().into_owned()).into(),
            source.to_string(),
        );

        let module = parse_file_as_module(
            &fm,
            detect_syntax(path),
            target,
            None,
            &mut vec![],
        )
        .map_err(|err| {
            let msg = err.into_kind().msg().to_string();
            Error::custom(format!("{}: {msg}", path.display()))
        })?;

        let mut program = Program::Module(module);

        // 0. optionally rewrite specifiers before anything else looks at them
        if let Some(map) = opts.rewrite.as_ref() {
            program.mutate(SpecifierRewriter { map });
        }

        // 1. erase the type system
        program.mutate(strip(unresolved_mark, top_level_mark));

        // 2. mark globals and deconflict shadowed identifiers
        program.mutate(resolver_pass(unresolved_mark, top_level_mark, typescript));

        // 3. lower modules to CommonJS so everything fits in one file
        if opts.commonjs && !dts {
            program.mutate(common_js(
                Resolver::default(),
                unresolved_mark,
                common_js::Config {
                    strict_mode: true,
                    no_interop: false,
                    ..Default::default()
                },
                common_js::FeatureFlag::default(),
            ));
        }

        // 4. prepend the helper functions the transforms above referenced
        // 4. make low-precedence expressions explicit for the printer
        program.mutate(Parenthesise);

        // 5. prepend the helper functions the transforms above referenced
        program.mutate(swc_helpers::inject_helpers(top_level_mark));

        let module = match &mut program {
            Program::Module(m) => m,
            Program::Script(_) => {
                return Err(Error::custom(format!(
                    "{} is not a module (top-level `return` or `with`?)",
                    path.display()
                )));
            }
        };

        let mut buf = Vec::new();
        let mut cfg = CodegenConfig::default();
        cfg.minify = opts.minify;
        cfg.target = target;
        let mut emitter = Emitter {
            cfg,
            cm: cm.clone(),
            comments: None,
            wr: JsWriter::new(cm.clone(), "\n", &mut buf, None),
        };
        emitter
            .emit_module(module)
            .map_err(|e| Error::custom(format!("codegen failed: {e}")))?;

        let code = String::from_utf8_lossy(&buf).into_owned();
        let line_count = code.lines().count() as u32;
        Ok(Compiled {
            code,
            line_count,
            is_declaration_file: dts,
        })
    })
    })
}

/// Knobs for a single compile.
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Lower `import`/`export` to `require`/`exports`.
    pub commonjs: bool,
    pub minify: bool,
    /// Replace module specifiers using this table before compiling.
    pub rewrite: Option<std::collections::BTreeMap<String, String>>,
}

/// Rewrites `import x from "a"` (and dynamic imports and `require`) in place.
struct SpecifierRewriter<'a> {
    map: &'a std::collections::BTreeMap<String, String>,
}

impl SpecifierRewriter<'_> {
    fn rewrite(&self, src: &mut Str) {
        let current = src.value.to_atom_lossy().to_string();
        if let Some(to) = self.map.get(&current) {
            src.value = swc_core::ecma::atoms::Wtf8Atom::new(to.as_str());
        }
    }

    /// Handle a call whose first argument is a string literal (`require("x")`).
    fn rewrite_call_args(&self, node: &mut CallExpr) {
        let Some(ExprOrSpread {
            spread: None,
            expr,
        }) = node.args.first_mut()
        else {
            return;
        };
        if let Expr::Lit(Lit::Str(s)) = &mut **expr {
            self.rewrite(s);
        }
    }

    fn is_require(callee: &Callee) -> bool {
        matches!(callee, Callee::Expr(e) if matches!(&**e, Expr::Ident(i) if i.sym.as_ref() == "require"))
    }
}

impl swc_core::ecma::ast::Pass for SpecifierRewriter<'_> {
    fn process(&mut self, program: &mut Program) {
        swc_core::ecma::visit::VisitMut::visit_mut_program(self, program);
    }
}

impl VisitMut for SpecifierRewriter<'_> {
    fn visit_mut_import_decl(&mut self, node: &mut ImportDecl) {
        self.rewrite(&mut node.src);
        swc_core::ecma::visit::VisitMutWith::visit_mut_children_with(node, self);
    }

    fn visit_mut_named_export(&mut self, node: &mut NamedExport) {
        if let Some(src) = node.src.as_deref_mut() {
            self.rewrite(src);
        }
        swc_core::ecma::visit::VisitMutWith::visit_mut_children_with(node, self);
    }

    fn visit_mut_export_all(&mut self, node: &mut ExportAll) {
        self.rewrite(&mut node.src);
        swc_core::ecma::visit::VisitMutWith::visit_mut_children_with(node, self);
    }

    fn visit_mut_call_expr(&mut self, node: &mut CallExpr) {
        if matches!(node.callee, Callee::Import(_)) || Self::is_require(&node.callee) {
            self.rewrite_call_args(node);
        }
        swc_core::ecma::visit::VisitMutWith::visit_mut_children_with(node, self);
    }
}

/// Strip types without lowering modules — used for `.d.ts` and for `--check`.
pub fn strip_types_only(cm: &Lrc<SourceMap>, path: &Path, source: &str) -> Result<String> {
    let compiled = compile_file(
        cm,
        path,
        source,
        EsVersion::EsNext,
        CompileOptions::default(),
    )?;
    Ok(compiled.code)
}

/// Wraps low-precedence expressions in explicit parentheses.
///
/// The module transform rewrites a call on an imported binding into the
/// sequence expression `(0, dep.fn)(arg)` so the module namespace is not
/// passed as `this`. When that sequence lands in an argument position the code
/// generator prints it without parentheses, which silently turns one argument
/// into two. Making the parentheses part of the AST removes the ambiguity.
struct Parenthesise;

impl swc_core::ecma::ast::Pass for Parenthesise {
    fn process(&mut self, program: &mut Program) {
        swc_core::ecma::visit::VisitMut::visit_mut_program(self, program);
    }
}

impl VisitMut for Parenthesise {
    fn visit_mut_expr(&mut self, node: &mut Expr) {
        let needs_parens = matches!(node, Expr::Seq(_) | Expr::Arrow(_));
        if needs_parens {
            let span = swc_core::common::DUMMY_SP;
            let inner = std::mem::replace(node, Expr::Invalid(Invalid { span }));
            *node = Expr::Paren(ParenExpr {
                span,
                expr: Box::new(inner),
            });
            return;
        }
        swc_core::ecma::visit::VisitMutWith::visit_mut_children_with(node, self);
    }
}

/// Collect every module specifier in a file: static imports, re-exports,
/// dynamic `import()` and CommonJS `require("…")` calls.
pub fn collect_specifiers(module: &swc_core::ecma::ast::Module) -> Vec<String> {
    use swc_core::ecma::visit::Visit;

    struct Specs(Vec<String>);

    impl Visit for Specs {
        fn visit_import_decl(&mut self, node: &ImportDecl) {
            self.0.push(node.src.value.to_atom_lossy().to_string());
            swc_core::ecma::visit::VisitWith::visit_children_with(node, self);
        }

        fn visit_named_export(&mut self, node: &NamedExport) {
            if let Some(src) = &node.src {
                self.0.push(src.value.to_atom_lossy().to_string());
            }
            swc_core::ecma::visit::VisitWith::visit_children_with(node, self);
        }

        fn visit_export_all(&mut self, node: &ExportAll) {
            self.0.push(node.src.value.to_atom_lossy().to_string());
            swc_core::ecma::visit::VisitWith::visit_children_with(node, self);
        }

        fn visit_call_expr(&mut self, node: &CallExpr) {
            let is_import = matches!(node.callee, Callee::Import(_));
            let is_require = SpecifierRewriter::is_require(&node.callee);
            if is_import || is_require {
                if let Some(ExprOrSpread {
                    spread: None,
                    expr,
                }) = node.args.first()
                    && let Expr::Lit(Lit::Str(s)) = &**expr
                {
                    self.0.push(s.value.to_atom_lossy().to_string());
                }
            }
            swc_core::ecma::visit::VisitWith::visit_children_with(node, self);
        }
    }

    let mut specs = Specs(Vec::new());
    specs.visit_module(module);
    specs.0.sort();
    specs.0.dedup();
    specs.0
}