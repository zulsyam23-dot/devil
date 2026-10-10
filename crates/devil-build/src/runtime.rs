//! The JavaScript runtime that ships inside every bundle.
//!
//! It is intentionally tiny: a module registry, a cache and a per-module
//! `require` that consults a table baked in at build time. Because resolution
//! happens at build time, the runtime never touches the filesystem.

/// Banner comment placed at the top of every artifact.
pub fn header(name: &str, module_count: usize) -> String {
    format!(
        "// -----------------------------------------------------------------------------\n\
         //  devil bundle  ·  {name}\n\
         //  {module_count} module(s) inlined, no node_modules required\n\
         // -----------------------------------------------------------------------------\n\n",
        name = name,
        module_count = module_count
    )
}

/// The registry + loader, returned as an IIFE body used by both the plain
/// bundle and the IIFE wrapper.
pub fn preamble() -> String {
    r#"var __devil_defs = Object.create(null);
var __devil_cache = Object.create(null);
var __devil_builtins = (typeof globalThis !== "undefined" && globalThis.__devil_builtins) || {};

function __devil_require(id) {
  if (Object.prototype.hasOwnProperty.call(__devil_cache, id)) {
    return __devil_cache[id].exports;
  }
  var def = __devil_defs[id];
  if (!def) {
    throw new Error("devil: module not found in bundle: " + id);
  }
  var module = __devil_cache[id] = { id: id, exports: {} };
  var table = def[1];
  function require(spec) {
    if (Object.prototype.hasOwnProperty.call(table, spec)) {
      return __devil_require(table[spec]);
    }
    var bare = spec.replace(/^node:/, "");
    if (Object.prototype.hasOwnProperty.call(__devil_builtins, bare)) {
      return __devil_builtins[bare];
    }
    if (spec.charAt(0) !== "." && spec.charAt(0) !== "/") {
      throw new Error(
        "devil: '" + spec + "' is not bundled and no runtime provides it"
      );
    }
    throw new Error("devil: cannot resolve '" + spec + "' from " + id);
  }
  require.resolve = function (spec) {
    return table[spec];
  };
  def[0].call(module.exports, module, module.exports, require);
  return module.exports;
}

"#
    .to_string()
}

/// Wrap one compiled module together with its resolution table.
pub fn define(
    id: &str,
    code: &str,
    deps: &std::collections::BTreeMap<String, String>,
    _builtins: &std::collections::BTreeSet<String>,
    _externals: &std::collections::BTreeSet<String>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "__devil_defs[{}] = [function (module, exports, require) {{\n",
        crate::bundle::js_string(id)
    ));
    for line in code.lines() {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!(
        "}}, {}];\n\n",
        crate::bundle::deps_object(deps)
    ));
    out
}

/// Globals every `devil run` environment installs before the bundle loads.
pub const HOST_GLOBALS: &str = r#"
globalThis.__devil_builtins = globalThis.__devil_builtins || {};
"#;