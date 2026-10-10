# Architecture

Six crates, layered so that each one can be tested on its own.

```text
devil-core      manifest, lockfile, config, paths, errors
    ├── devil-package     .devil archive: pack, verify, extract
    ├── devil-registry    HTTP client for the registry protocol
    ├── devil-resolver    dependency resolution + the .devil store
    │       └── devil-build    TypeScript → JavaScript, then bundling
    └── devil-vm          embedded QuickJS + host globals
            └── devil-cli     argument parsing and command implementations
```

---

## devil-core

Everything the rest of the tool agrees on.

- **`manifest`** — `devil.toml`. Deserialised with `serde`; dependencies accept
  either a bare semver string or an inline table, so `foo = "^1"` and
  `foo = { version = "1" }` are the same thing. Unknown keys are rejected.
- **`lock`** — `devil.lock`. Two parts: a checksum per resolved package, and a
  per-edge table mapping each dependent to the exact version it sees.
- **`paths`** — the layout that replaces `node_modules`. `Dir::discover` walks
  up from the current directory to the nearest `devil.toml`; `Dir::store`
  returns `.devil/store/<name>-<version>`.
- **`config`** — `~/.config/devil/config.toml`, overridable by `DEVIL_REGISTRY`,
  `DEVIL_TOKEN` and `DEVIL_OFFLINE`.

---

## devil-package

The `.devil` format: a gzipped tar with a single top-level directory, exactly
like Cargo's `.crate`.

```text
greeter-0.1.0.devil
└── greeter-0.1.0/
    ├── devil.toml
    ├── .devil-manifest.json
    └── src/…
```

Three properties matter:

1. **Determinism.** Members are sorted, mtimes are pinned to 1980-01-01, uid/gid
   are zeroed, permissions are normalised to `0644`. The same tree always packs
   to the same bytes, so the checksum is reproducible and the lockfile is
   meaningful.
2. **Self-description.** `.devil-manifest.json` carries the resolved
   dependency list, so the resolver never extracts a package just to read its
   manifest.
3. **Safety.** Extraction rejects any member whose path escapes the destination.

Include and exclude globs are matched by a small matcher supporting `*`, `**`
and `?`.

---

## devil-registry

A blocking client for the five protocol endpoints, plus `PublishRequest`,
`YankRequest` and the two response documents. `ureq` with rustls, a global
timeout, and bearer-token headers.

`Registry::index` maps `404` to an empty index so callers can report
`no version of X matches Y; candidates: …` instead of a bare HTTP error.

A read-only registry needs no implementation: `devil registry serve` in the CLI
serves the static `index/` + `dl/` layout over plain `TcpListener`.

---

## devil-resolver

**Resolution.** A breadth-first walk over `(dependent, dependency, requirement)`
triples. For each, it picks the highest matching version, preferring the one
already pinned in the lockfile so builds stay reproducible. Yanked versions are
skipped unless nothing else fits.

Dependencies come from the index entry, never from an extracted package, so
resolution costs one HTTP request per package name rather than per package.

Feature handling is deliberately modest: `default` plus every feature named by
a dependent enables the optional dependency of the same name.

**The store.** `.devil/store/<name>-<version>/`, flat. No nesting, no hoisting,
no duplication. Two dependents needing different versions of the same package
simply get two directories.

**Module resolution** (`module.rs`) turns a bare specifier into a file. It asks
the lockfile which version the *importing* package sees, then probes the package
directory for the subpath, trying `<pkg>/<subpath>` and `<pkg>/src/<subpath>`
with extensions `.ts .tsx .mts .cts .js .jsx .mjs .cjs .json` and then
`index.*`. Runtime builtins and externals short-circuit before any filesystem
work.

Module ids are the workspace-relative path, with store paths kept as
`.devil/store/<name>-<version>/…` so two versions can never collide in the
bundle.

---

## devil-build

**transpile.** Parse with swc, then four explicit passes:

```text
parse ─▶ strip ─▶ resolver ─▶ common_js ─▶ parenthesise ─▶ inject_helpers ─▶ emit
```

Every pass is named because the output is meant to be readable when something
goes wrong.

`parenthesise` is a workaround worth knowing about: the module transform turns
a call on an imported binding into the sequence `(0, dep.fn)(arg)` so the
namespace is not passed as `this`, and the code generator prints that sequence
without parentheses in argument position — silently turning one argument into
two. Wrapping every `SeqExpr` and arrow in an explicit `ParenExpr` makes the
intent part of the AST rather than the printer's job.

Helpers (`_interop_require_default`, …) are inlined, not imported, so a bundle
never references `@swc/helpers`.

**bundle.** A breadth-first walk from the entry. Each module's specifiers are
collected *before* compiling, so a broken import fails with a readable message
instead of a codegen error. The result is a tiny module registry:

```js
__devil_defs["src/index.ts"] = [
  function (module, exports, require) { /* compiled code */ },
  { "./greet.js": "src/greet.ts", "greeter": ".devil/store/greeter-0.1.0/src/index.ts" }
];
```

Each module gets its own `require`, which consults its table and falls back to
the host's builtins. That is what allows two versions of a package to live in
one file without colliding.

JSON modules become `module.exports = <literal>`.

---

## devil-vm

QuickJS linked into the binary. The host installs `console`, `process`, `fs`,
`path`, `os`, `url`, `util`, `events`, `assert`, `Buffer`, `crypto`, `timers`,
`fetch` and friends: Rust-backed functions where the work is real (`fs`, the
HTTP round trip, hashing, entropy), and small JavaScript shims layered on top
where it is not.

Two constraints shaped this crate:

- **Field order in `Vm` is load-bearing.** The context must be dropped before
  the runtime; Rust drops struct fields in declaration order.
- **Host functions must not return Rust-owned objects.** Handing an `Object` to
  JavaScript leaves it referenced by the runtime past teardown, and QuickJS
  aborts on `list_empty(&rt->gc_obj_list)`. Host functions return JSON strings
  and the shims parse them. `tests/quickjs_shutdown.rs` guards this.

Timers are driven by a poll loop: Rust asks the shim to run whatever is due and
reports how long to sleep next, which keeps `setTimeout` honest without an
async runtime.

---

## devil-cli

`clap` with one module per command group. No framework, no plugin system —
each command is a function that opens a workspace and does one thing.

`to_anyhow` in `cmd/build.rs` is the single conversion point between the typed
error in `devil-core` and `anyhow` at the top level, so error messages reach the
user unchanged.

---

## Deliberate limitations

- Resolution is not the full Cargo algorithm: there is no backtracking across
  feature combinations, and one version is chosen per `(dependent, name)` pair.
- `fetch` blocks; there are no streaming response bodies.
- Dev-dependencies of dependencies are skipped entirely.
- No lockfile format migration yet — the `format` field is written but not read.
- Circular dependencies resolve but the bundler reports the cycle only as a
  depth-first traversal, not a diagnostic.