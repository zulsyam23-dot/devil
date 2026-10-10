//! Compilation and bundling for `devil`.
//!
//! Everything here is pure Rust: TypeScript is parsed and erased with swc,
//! and the module graph is linked by our own resolver. There is no Node.js, no
//! `esbuild` binary and no `node_modules` anywhere in the pipeline.

pub mod bundle;
pub mod runtime;
pub mod transpile;

pub use bundle::{build, BuildOptions, Bundle, Format, ModuleRecord, Platform};
pub use transpile::{compile_file, CompileOptions};
pub use swc_core::ecma::ast::EsVersion;