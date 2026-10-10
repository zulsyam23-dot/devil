# devil

**A package manager and toolchain for TypeScript, written entirely in Rust.**
Cargo's model — a manifest, a lockfile, a content store, deterministic archives —
applied to TypeScript libraries. No Node.js, no `npm`, and **no `node_modules`
directory anywhere**.

```sh
devil init my-app --bin
devil add greeter
devil run
```

---

## Why it exists

`npm` installs packages by copying files into nested `node_modules` trees. The
result is slow, hard to reproduce, and hard to reason about. `devil` works the
way Cargo does:

| npm                    | devil                    |
| ---------------------- | ------------------------ |
| `package.json`         | `devil.toml`             |
| `package-lock.json`    | `devil.lock`             |
| `node_modules/`        | `.devil/store/`          |
| `~/.npm/_cacache`      | `.devil/cache/`          |
| `*.tgz`                | `*.devil`                |
| `npm install`          | `devil install`          |
| `npm publish`          | `devil publish`          |
| `node` + `esbuild`     | embedded QuickJS + swc   |

Two versions of the same package simply get two directories in the store. The
lockfile records which version each dependent actually sees, so there is no
hoisting pass, no nesting and no duplicated files.

---

## Install

```sh
cargo install --path crates/devil-cli
```

Requires Rust 1.85 or newer. The resulting binary is self-contained: the
TypeScript compiler, the bundler and the JavaScript engine all live inside it.

---

## The manifest

```toml
[package]
name = "greeter"
version = "0.1.0"
edition = "es2022"            # language target for the type-stripping pass
description = "Friendly greetings."
license = "MIT"
entry = "src/index.ts"        # defaults to src/index.ts
include = ["src", "README.md"]

[dependencies]
greeter = "^0.1"
tiny-log = { version = "0.3", features = ["ansi"] }

[dev-dependencies]
tsd = "^1.0"

[features]
default = ["ansi"]
ansi = []

[profile.release]
format = "iife"
minify = true
external = []
```

A dependency is either a semver range (`"^1.2"`) or an inline table with
`version`, `features`, `default_features`, `registry`, `git`/`rev` or `path`.

---

## The `.devil` format

A `.devil` file is a gzipped tar, exactly like Cargo's `.crate`:

```text
greeter-0.1.0.devil  (gzip)
└── greeter-0.1.0/
    ├── devil.toml             # the manifest, normalised
    ├── .devil-manifest.json   # metadata the resolver reads
    ├── README.md
    └── src/
        ├── index.ts
        └── greet.ts
```

Archives are written **deterministically**: members are sorted, timestamps and
ownership are zeroed, permissions are normalised. Packing the same tree twice
always produces byte-identical output, and therefore the same
`sha256-<hex>` checksum. That is what makes `devil.lock` trustworthy.

---

## On-disk layout

```text
my-app/
├── devil.toml
├── devil.lock
├── src/
├── dist/                     # build output
└── .devil/                   # replaces node_modules entirely
    ├── cache/greeter-0.1.0.devil
    ├── store/greeter-0.1.0/  # extracted sources
    └── graph.json
```

---

## Commands

| Command                | What it does                                              |
| ---------------------- | --------------------------------------------------------- |
| `devil init [name]`    | Scaffold `devil.toml`, `src/index.ts`, README, `.gitignore` |
| `devil add <pkg>`      | Add a dependency and install it                            |
| `devil remove <pkg>`   | Remove it (`--purge` also drops it from the store)          |
| `devil install`        | Resolve, download, extract, write the lockfile              |
| `devil sync`           | Alias for `install`, for CI                                 |
| `devil update [pkg]`   | Move to the newest version the requirement allows           |
| `devil tree`           | Show the resolved graph                                     |
| `devil info <pkg>`     | Registry metadata, or `--local` for the store                |
| `devil build`          | Bundle to `dist/`                                           |
| `devil run [file]`     | Execute on the embedded QuickJS runtime                     |
| `devil test`           | Run every `*.test.ts` with a built-in harness               |
| `devil check`          | Parse and compile every source file                          |
| `devil pack`           | Produce a `.devil` archive                                  |
| `devil publish`        | Upload to a registry                                        |
| `devil yank <ver>`     | Mark a published version broken                             |
| `devil search <query>` | Search the registry                                         |
| `devil vendor`         | Copy dependencies into a plain directory                    |
| `devil clean`          | Empty the store (`--all` also drops cached archives)         |
| `devil login`          | Store a registry API token                                  |
| `devil registry serve` | Run a local read-only registry                              |

Global flags: `--registry`, `--offline`, `--verbose`. Environment:
`DEVIL_HOME`, `DEVIL_REGISTRY`, `DEVIL_TOKEN`, `DEVIL_OFFLINE`, `NO_COLOR`.

---

## Building

```sh
devil build                       # dist/my-app.js        (CommonJS)
devil build --format iife --name MyApp
devil build --format esm          # dist/esm/index.mjs + one file per module
devil build --release --minify
devil build --platform browser --external react
```

Output formats: `cjs` (default), `iife`, `esm`, `app`.

The pipeline is explicit and readable:

```text
source ─▶ parse ─▶ strip types ─▶ resolve identifiers ─▶ CommonJS ─▶ link
```

Resolution happens at build time. Each module keeps its own `require` closure
backed by a table baked into the bundle, so the runtime never touches the
filesystem and two versions of the same package can coexist in one file.

---

## Running without Node.js

```sh
devil run                       # uses package.entry
devil run src/cli.ts -- --verbose
devil run -e "console.log(1 + 1)"
```

`devil run` executes the bundle on QuickJS, embedded in the CLI. The runtime
installs `console`, `process`, `fs`, `path`, `os`, `url`, `URL`,
`URLSearchParams`, `util`, `events`, `assert`, `Buffer`, `TextEncoder`,
`TextDecoder`, `btoa`/`atob`, `crypto`, `timers`, `fetch`, `performance`,
`queueMicrotask` and `structuredClone`.

`fetch` performs one blocking round trip per call — enough for scripts and
tools, not enough for a server. `net`, `zlib` and `stream` are present as
minimal placeholders.

---

## Testing

There is no runner to install, so `devil` ships one:

```ts
import { hello } from "./index.js";

test("greets", () => {
  assertEquals(hello("world"), "hello, world");
});
```

```sh
devil test            # discovers tests/*.test.ts and src/**/*.test.ts
devil test --filter greet
```

---

## Resolution model

The resolver is Cargo-like. For each `(dependent, dependency, requirement)`
triple it picks the highest version that matches, preferring the version
already pinned in `devil.lock` so builds stay reproducible. Different dependents
may resolve the same name to different versions; each gets its own store
directory, and the per-edge table in the lockfile tells the bundler which one to
use.

Optional dependencies are pulled in when a feature of the same name is enabled.
Dev-dependencies of dependencies are never resolved.

---

## Registry protocol

A registry is any HTTP server that speaks five endpoints. It can be a CDN, an
object store, or the built-in `devil registry serve`.

See [REGISTRY.md](./REGISTRY.md) for the full specification.

---

## Self-hosting a registry

```sh
devil pack --out greeter-0.1.0.devil
devil registry index greeter-0.1.0.devil --dir ./registry
devil registry serve --dir ./registry --addr 127.0.0.1:8080
```

```sh
DEVIL_REGISTRY=http://127.0.0.1:8080 devil add greeter
```

---

## License

MIT