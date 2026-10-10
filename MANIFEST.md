# Manifest reference

`devil.toml` is the package manifest. It follows `Cargo.toml` closely, because
every idea in it — a package, a version, dependencies with semver ranges,
optional dependencies switched on by features, a publish registry — comes from
Cargo rather than from `package.json`.

Unknown keys are rejected rather than ignored, so a typo is an error instead of
a silent no-op.

---

## `[package]`

| Key            | Type     | Default          | Notes                                      |
| -------------- | -------- | ---------------- | ------------------------------------------ |
| `name`         | string   | required         | lowercase ASCII, digits, `-`, `_`          |
| `version`      | string   | required         | semver                                     |
| `edition`      | string   | `es2022`         | `es2015`…`es2023`, `esnext`                 |
| `description`  | string   | —                | required by `publish`                       |
| `authors`      | string[] | `[]`             |                                            |
| `license`      | string   | —                | required by `publish`                       |
| `repository`   | string   | —                |                                            |
| `homepage`     | string   | —                |                                            |
| `documentation`| string   | —                |                                            |
| `keywords`     | string[] | `[]`             |                                            |
| `categories`   | string[] | `[]`             |                                            |
| `readme`       | string   | `README.md`      | shipped in the archive                      |
| `entry`        | string   | `src/index.ts`   | entry module                                |
| `include`      | string[] | `["src", "README.md", "LICENSE*"]` | globs shipped in the archive |
| `exclude`      | string[] | `[]`             | never shipped                                |
| `publish`      | bool\|str\|table | `true`  | `false` disables publishing                 |

`edition` selects the ECMAScript target the type-stripping pass preserves.
`esnext` keeps modern syntax intact; `es2022` is the default and matches every
currently supported engine, including the bundled QuickJS.

`publish` accepts three shapes:

```toml
publish = false                        # refuse to publish
publish = "https://my.registry"        # publish there
publish = { registry = "https://…" }   # same, explicitly
```

### Glob patterns

`include` and `exclude` support `*` (no `/`), `**` (any depth), `?` and bare
directory names, which match everything beneath them:

```toml
include = ["src", "README.md", "docs/**"]
exclude = ["src/**/*.test.ts"]
```

---

## Dependencies

```toml
[dependencies]
plain = "^1.2"
with-options = { version = "0.3", features = ["ansi"], default-features = false }
elsewhere = { version = "2", registry = "corp" }
```

| Key                | Type     | Default | Notes                          |
| ------------------ | -------- | ------- | ------------------------------ |
| `version`          | string   | `*`     | semver requirement             |
| `features`         | string[] | `[]`    | requested feature set          |
| `default-features` | bool     | `true`  |                                |
| `registry`         | string   | default | name from `[registries]` or a URL |
| `git` / `rev`      | string   | —       | fetch from a git remote        |
| `path`             | string   | —       | use a local directory          |

Three tables exist: `[dependencies]`, `[dev-dependencies]` and
`[peer-dependencies]`. Dev-dependencies are installed but never resolved
transitively. Peer-dependencies are recorded and shipped; they are not fetched
automatically.

---

## Features

```toml
[features]
default = ["ansi"]
ansi = []
verbose = ["tiny-log/verbose"]
```

A feature listing a bare name switches on the optional dependency of that name
or another feature of the same package. A feature listing `pkg/feature` switches
on a feature of a dependency.

`default` is enabled unless a dependency sets `default-features = false`.

---

## Registries

```toml
[registries.corp]
registry = "https://corp.registry/angel"
token-env = "CORP_DEVIL_TOKEN"
```

`token-env` names an environment variable holding the API token for that
registry. Without it, `DEVIL_TOKEN` is used.

---

## `[[bin]]`

```toml
[[bin]]
name = "my-app"
path = "src/main.ts"
```

`devil pack` refuses to publish if a bin target is missing, and the bundler
uses it as the default entry for `--format app`.

---

## `[profile.<name>]`

```toml
[profile.release]
format = "iife"
minify = true
sourcemap = false
external = ["react"]
inline = ["tiny-log"]
```

| Key         | Type     | Notes                                              |
| ----------- | -------- | -------------------------------------------------- |
| `format`    | string   | `cjs`, `iife`, `esm`, `app`                          |
| `minify`    | bool     |                                                      |
| `sourcemap` | bool     |                                                      |
| `external`  | string[] | left as bare imports instead of being inlined        |
| `inline`    | string[] | force these packages to be inlined, even if external |

`devil build --release` uses `[profile.release]`; everything else uses
`[profile.dev]`. Command-line flags win over both.

---

## `devil.lock`

Generated, never edited by hand.

```toml
version = 3
format = "devil-lock-v3"
root = "my-app@0.1.0"

[packages]
greeter-0.1.0 = "sha256-5918…"
tiny-log-0.3.2 = "sha256-1f04…"

[graph."my-app@0.1.0"]
greeter = "0.1.0"

[graph."greeter@0.1.0"]
tiny-log = "0.3.2"
```

`[packages]` pins checksums. `[graph]` is the per-edge resolution table: for each
dependent, the exact version it sees. The bundler reads it to map a bare
specifier to a directory in the store, which is what makes a flat store behave
like a nested tree.