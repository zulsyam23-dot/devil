//! Host objects installed into the QuickJS context.
//!
//! QuickJS ships the language but none of the platform. Everything a normal
//! TypeScript program expects from its host — `console`, `process`, `fs`,
//! `path`, `fetch` — is installed here, either as a Rust-backed function or as
//! a small JavaScript shim layered on top of one.

use std::path::PathBuf;
use std::sync::Arc;

use rquickjs::function::{Opt, Rest};
use rquickjs::{Function, Object};

use super::HostState;

use super::vm_err;
use devil_core::Result;

/// Install every builtin into `ctx`.
pub fn install(ctx: &rquickjs::Ctx<'_>, host: &Arc<HostState>) -> Result<()> {
    let globals = ctx.globals();

    // The shims describe the host through these two globals.
    globals
        .set("__devil_host_platform", host.platform.clone())
        .map_err(vm_err)?;
    globals
        .set("__devil_host_arch", host.arch.clone())
        .map_err(vm_err)?;

    install_host_functions(ctx, &globals, host)?;
    install_fs(ctx, &globals, host)?;

    for (label, source) in [
        ("console", SHIM_CONSOLE),
        ("process", SHIM_PROCESS),
        ("path", SHIM_PATH),
        ("os", SHIM_OS),
        ("util", SHIM_UTIL),
        ("events", SHIM_EVENTS),
        ("url", SHIM_URL),
        ("buffer", SHIM_BUFFER),
        ("crypto", SHIM_CRYPTO),
        ("misc", SHIM_MISC),
        ("timers", SHIM_TIMERS),
        ("module-table", SHIM_TABLE),
    ] {
        ctx.eval::<(), _>(source).map_err(|e| {
            super::vm_err(format!(
                "shim `{label}` failed to install: {e}"
            ))
        })?;
    }
    Ok(())
}

fn js_error(to: &'static str, message: impl std::fmt::Display) -> rquickjs::Error {
    rquickjs::Error::new_from_js_message("devil-vm", to, message.to_string())
}

fn cwd_of(st: &Arc<HostState>) -> PathBuf {
    st.cwd.lock().map(|c| c.clone()).unwrap_or_default()
}

fn resolve_arg(st: &Arc<HostState>, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() { p } else { cwd_of(st).join(p) }
}

fn install_host_functions<'js>(
    ctx: &rquickjs::Ctx<'js>,
    globals: &Object<'js>,
    host: &Arc<HostState>,
) -> Result<()> {
    let st = host.clone();
    globals
        .set(
            "__devil_host_cwd",
            Function::new(ctx.clone(), move || -> String {
                cwd_of(&st).display().to_string()
            })
            .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    let st = host.clone();
    globals
        .set(
            "__devil_host_argv",
            Function::new(ctx.clone(), move || -> Vec<String> {
                st.argv.lock().map(|a| a.clone()).unwrap_or_default()
            })
            .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    let st = host.clone();
    globals
        .set(
            "__devil_host_exit",
            Function::new(ctx.clone(), move |code: Opt<i32>| {
                if let Ok(mut slot) = st.exit_code.lock() {
                    *slot = Some(code.0.unwrap_or(0));
                }
                st.exited.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    globals
        .set(
            "__devil_host_env",
            Function::new(ctx.clone(), |name: String| std::env::var(name).ok())
                .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    globals
        .set("__devil_host_env_json", env_json())
        .map_err(vm_err)?;

    globals
        .set(
            "__devil_host_random_hex",
            Function::new(ctx.clone(), |len: Opt<usize>| {
                let mut buf = vec![0u8; len.0.unwrap_or(16)];
                fill_random(&mut buf);
                hex::encode(buf)
            })
            .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    globals
        .set(
            "__devil_host_now",
            Function::new(ctx.clone(), || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64() * 1000.0)
                    .unwrap_or(0.0)
            })
            .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    globals
        .set(
            "__devil_host_sha256",
            Function::new(ctx.clone(), |input: String| {
                use sha2::Digest;
                let mut hasher = sha2::Sha256::new();
                sha2::Digest::update(&mut hasher, input.as_bytes());
                hex::encode(sha2::Digest::finalize(hasher))
            })
            .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    globals
        .set(
            "__devil_host_http",
            Function::new(
                ctx.clone(),
                |args: Rest<String>| -> Result<String, rquickjs::Error> {
                    let method = args.first().cloned().unwrap_or_default();
                    let url = args.get(1).cloned().unwrap_or_default();
                    let headers = args.get(2).cloned().unwrap_or_default();
                    let body = args.get(3).cloned().unwrap_or_default();
                    http_roundtrip(&method, &url, &headers, &body)
                },
            )
            .map_err(vm_err)?,
        )
        .map_err(vm_err)?;

    Ok(())
}

fn install_fs<'js>(
    ctx: &rquickjs::Ctx<'js>,
    globals: &Object<'js>,
    host: &Arc<HostState>,
) -> Result<()> {
    let fs = Object::new(ctx.clone()).map_err(vm_err)?;
    globals.set("__devil_host_fs", fs.clone()).map_err(vm_err)?;

    let st = host.clone();
    fs.set(
        "readFileSync",
        Function::new(ctx.clone(), move |path: String| -> rquickjs::Result<String> {
            let full = resolve_arg(&st, &path);
            std::fs::read_to_string(&full)
                .map_err(|e| js_error("fs.readFileSync", format!("{}: {e}", full.display())))
        })
        .map_err(vm_err)?,
    )
    .map_err(vm_err)?;

    let st = host.clone();
    fs.set(
        "writeFileSync",
        Function::new(ctx.clone(), move |path: String, data: String| -> rquickjs::Result<()> {
            let full = resolve_arg(&st, &path);
            if let Some(parent) = full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&full, data)
                .map_err(|e| js_error("fs.writeFileSync", format!("{}: {e}", full.display())))
        })
        .map_err(vm_err)?,
    )
    .map_err(vm_err)?;

    let st = host.clone();
    fs.set(
        "existsSync",
        Function::new(ctx.clone(), move |path: String| resolve_arg(&st, &path).exists())
            .map_err(vm_err)?,
    )
    .map_err(vm_err)?;

    let st = host.clone();
    fs.set(
        "readdirSync",
        Function::new(ctx.clone(), move |path: String| -> rquickjs::Result<Vec<String>> {
            let full = resolve_arg(&st, &path);
            let mut names: Vec<String> = std::fs::read_dir(&full)
                .map_err(|e| js_error("fs.readdirSync", format!("{}: {e}", full.display())))?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Ok(names)
        })
        .map_err(vm_err)?,
    )
    .map_err(vm_err)?;

    let st = host.clone();
    fs.set(
        "mkdirSync",
        Function::new(ctx.clone(), move |path: String| -> rquickjs::Result<()> {
            let full = resolve_arg(&st, &path);
            std::fs::create_dir_all(&full)
                .map_err(|e| js_error("fs.mkdirSync", format!("{}: {e}", full.display())))
        })
        .map_err(vm_err)?,
    )
    .map_err(vm_err)?;

    let st = host.clone();
    fs.set(
        "rmSync",
        Function::new(ctx.clone(), move |path: String, options: Opt<bool>| -> rquickjs::Result<()> {
            let full = resolve_arg(&st, &path);
            let force = options.0.unwrap_or(false);
            let result = if full.is_dir() {
                std::fs::remove_dir_all(&full)
            } else {
                std::fs::remove_file(&full)
            };
            match result {
                Ok(()) => Ok(()),
                Err(_) if force => Ok(()),
                Err(e) => Err(js_error("fs.rmSync", format!("{}: {e}", full.display()))),
            }
        })
        .map_err(vm_err)?,
    )
    .map_err(vm_err)?;

    // `statSync` returns JSON rather than a host object: handing a Rust-owned
// object to JavaScript keeps it alive past the context teardown.
    let st = host.clone();
    fs.set(
        "statJson",
        Function::new(ctx.clone(), move |path: String| -> rquickjs::Result<String> {
            let full = resolve_arg(&st, &path);
            let meta = std::fs::metadata(&full)
                .map_err(|e| js_error("fs.statSync", format!("{}: {e}", full.display())))?;
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            Ok(serde_json::json!({
                "isFile": meta.is_file(),
                "isDirectory": meta.is_dir(),
                "size": meta.len(),
                "mtimeMs": modified,
            })
            .to_string())
        })
        .map_err(vm_err)?,
    )
    .map_err(vm_err)?;

    Ok(())
}

fn fill_random(buf: &mut [u8]) {
    use sha2::Digest;
    let mut counter: u64 = 0;
    let mut written = 0;
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    while written < buf.len() {
        let mut hasher = sha2::Sha256::new();
        sha2::Digest::update(&mut hasher, b"devil-vm-entropy");
        sha2::Digest::update(&mut hasher, &counter.to_le_bytes());
        sha2::Digest::update(&mut hasher, &wall.to_le_bytes());
        sha2::Digest::update(
            &mut hasher,
            &std::process::id().to_le_bytes(),
        );
        let digest = sha2::Digest::finalize(hasher);
        let take = buf.len().min(digest.len());
        buf[written..written + take].copy_from_slice(&digest[..take]);
        written += take;
        counter += 1;
    }
}

/// A single blocking HTTP round trip, marshalled as JSON.
fn http_roundtrip(
    method: &str,
    url: &str,
    headers: &str,
    body: &str,
) -> Result<String, rquickjs::Error> {
    let parsed: serde_json::Value = serde_json::from_str(headers).unwrap_or_default();
    let upper = method.to_uppercase();

    if matches!(upper.as_str(), "GET" | "HEAD" | "OPTIONS" | "") {
        let mut req = ureq::get(url).header("User-Agent", "devil-runtime");
        if let Some(map) = parsed.as_object() {
            for (k, v) in map {
                if let Some(text) = v.as_str() {
                    req = req.header(k, text);
                }
            }
        }
        return finish(req.call());
    }

    let mut req = ureq::post(url).header("User-Agent", "devil-runtime");
    if let Some(map) = parsed.as_object() {
        for (k, v) in map {
            if let Some(text) = v.as_str() {
                req = req.header(k, text);
            }
        }
    }
    finish(req.send(body.as_bytes()))
}

fn finish(response: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<String, rquickjs::Error> {
    match response {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp
                .into_body()
                .read_to_string()
                .unwrap_or_else(|_| String::new());
            Ok(serde_json::json!({ "status": status, "body": text }).to_string())
        }
        Err(ureq::Error::StatusCode(code)) => Ok(
            serde_json::json!({ "status": code, "body": "", "error": "http status" }).to_string(),
        ),
        Err(e) => Err(js_error("fetch", e)),
    }
}
// ---------------------------------------------------------------------------
// JavaScript shims
//
// Each string installs one piece of the platform onto `globalThis`. They run in
// dependency order: console and process first, then the pure-JS libraries, and
// finally the table that republishes them under the names a bundle expects.
// ---------------------------------------------------------------------------

const SHIM_CONSOLE: &str = r#"
(function () {
  const fmt = (v) => {
    if (typeof v === "string") return v;
    if (v instanceof Error) return v.stack || String(v);
    if (typeof v === "undefined") return "undefined";
    if (v === null) return "null";
    if (typeof v === "object") {
      try { return JSON.stringify(v, null, 2); } catch (_) { return String(v); }
    }
    return String(v);
  };
  const out = (...args) => __devil_print(args.map(fmt).join(" "));
  const err = (...args) => __devil_print_err(args.map(fmt).join(" "));
  const counters = new Map();
  globalThis.console = {
    log: out, info: out, debug: out, dir: out,
    warn: err, error: err, trace: (...a) => err("Trace:", ...a),
    count: (label = "default") => {
      const n = (counters.get(label) || 0) + 1;
      counters.set(label, n);
      out(label + ": " + n);
    },
    table: (v) => out(fmt(v)),
    group: () => {}, groupEnd: () => {},
    time: () => {}, timeEnd: () => {}, timeLog: () => {},
    assert: (cond, ...rest) => { if (!cond) err("Assertion failed:", ...rest); },
    clear: () => {},
  };
})();
"#;

const SHIM_PROCESS: &str = r#"
(function () {
  const argv = __devil_host_argv();
  const env = JSON.parse(__devil_host_env_json || "{}");
  const print = (s) => __devil_print(s);
  const printerr = (s) => __devil_print_err(s);
  globalThis.process = {
    argv: [argv[0] || "devil"].concat(argv.slice(1)),
    argv0: argv[0] || "devil",
    env: env,
    platform: __devil_host_platform,
    arch: __devil_host_arch,
    version: "v22.0.0-devil",
    versions: { devil: "0.1.0", quickjs: "embedded" },
    cwd: () => __devil_host_cwd(),
    chdir: () => { throw new Error("process.chdir is not supported by the devil runtime"); },
    exit: (code) => { __devil_host_exit(typeof code === "number" ? code : 0); },
    uptime: () => Math.floor(__devil_host_now() / 1000),
    hrtime: (prev) => { const n = Math.floor(__devil_host_now()); return prev ? [n - prev[0], 0] : [0, 0]; },
    memoryUsage: () => ({ rss: 0, heapTotal: 0, heapUsed: 0 }),
    nextTick: (fn) => queueMicrotask(fn),
    on: () => process, once: () => process, off: () => process, emit: () => false,
    stdout: { write: (s) => print(String(s).replace(/\n$/, "")) },
    stderr: { write: (s) => printerr(String(s).replace(/\n$/, "")) },
  };
  globalThis.global = globalThis;
  globalThis.globalThis.global = globalThis;
})();
"#;

const SHIM_PATH: &str = r#"
(function () {
  const cwd = __devil_host_cwd();
  const isWin = /^[a-zA-Z]:[\\/]/.test(cwd);
  const sep = isWin ? "\\" : "/";

  const normalise = (input) => {
    const p = String(input).replace(/\\/g, "/");
    const abs = p.startsWith("/");
    const parts = [];
    for (const seg of p.split("/")) {
      if (seg === "" || seg === ".") continue;
      if (seg === "..") {
        if (parts.length && parts[parts.length - 1] !== "..") parts.pop();
        else if (!abs) parts.push("..");
        continue;
      }
      parts.push(seg);
    }
    let out = parts.join("/");
    if (abs) out = "/" + out;
    if (isWin && /^[a-zA-Z]:/.test(out)) out = out[0].toUpperCase() + out.slice(1);
    return out || (abs ? "/" : ".");
  };

  const resolve = (...parts) => {
    let out = "";
    for (const p of parts) {
      if (!p) continue;
      out = p.startsWith("/") || /^[a-zA-Z]:/.test(p) ? normalise(p) : normalise(out + "/" + p);
    }
    return out || normalise(cwd);
  };

  const basename = (p, ext) => {
    const base = normalise(p).slice(normalise(p).lastIndexOf("/") + 1);
    if (ext && base.endsWith(ext)) return base.slice(0, -ext.length);
    return base;
  };
  const extname = (p) => {
    const base = basename(p);
    const i = base.lastIndexOf(".");
    return i <= 0 ? "" : base.slice(i);
  };
  const dirname = (p) => {
    const n = normalise(p);
    const i = n.lastIndexOf("/");
    return i <= 0 ? "/" : n.slice(0, i);
  };

  globalThis.__devil_path = {
    sep, delimiter: isWin ? ";" : ":", win32: isWin, posix: !isWin,
    normalize: normalise, resolve, join: (...p) => normalise(p.filter(Boolean).join("/")),
    dirname, basename, extname,
    relative: (from, to) => {
      const a = resolve(from).split("/").filter(Boolean);
      const b = resolve(to).split("/").filter(Boolean);
      let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      return [...a.slice(i).map(() => ".."), ...b.slice(i)].join("/") || ".";
    },
    isAbsolute: (p) => String(p).startsWith("/") || /^[a-zA-Z]:/.test(String(p)),
    parse: (p) => {
      const base = basename(p);
      const ext = extname(p);
      return {
        root: isWin ? dirname(p).slice(0, 2) : "/",
        dir: dirname(p), base, ext, name: ext ? base.slice(0, -ext.length) : base,
      };
    },
    format: (o) => (o.dir || "") + (o.base || ""),
  };
})();
"#;

const SHIM_OS: &str = r#"
(function () {
  const path = globalThis.__devil_path;
  globalThis.__devil_os = {
    platform: __devil_host_platform,
    arch: __devil_host_arch,
    EOL: "\n",
    sep: path.sep,
    homedir: () => __devil_host_env("HOME") || __devil_host_env("USERPROFILE") || "/",
    tmpdir: () => __devil_host_env("TMP") || __devil_host_env("TEMP") || "/tmp",
    hostname: () => __devil_host_env("COMPUTERNAME") || __devil_host_env("HOSTNAME") || "localhost",
    release: () => "embedded",
    cpus: () => [], totalmem: () => 0, freemem: () => 0,
    uptime: () => Math.floor(__devil_host_now() / 1000),
  };
})();
"#;

const SHIM_UTIL: &str = r#"
(function () {
  const seen = new WeakSet();
  const inspect = (v, depth = 0) => {
    if (typeof v === "string") return depth === 0 ? v : JSON.stringify(v);
    if (typeof v === "function") return v.name ? "[Function: " + v.name + "]" : "[Function]";
    if (typeof v === "symbol") return v.toString();
    if (v === null) return "null";
    if (v === undefined) return "undefined";
    if (typeof v !== "object") return String(v);
    if (seen.has(v)) return "[Circular]";
    if (depth > 4) return Array.isArray(v) ? "[Array]" : "[Object]";
    seen.add(v);
    let out;
    if (Array.isArray(v)) out = "[ " + v.map((x) => inspect(x, depth + 1)).join(", ") + " ]";
    else {
      const keys = Object.keys(v);
      out = keys.length
        ? "{ " + keys.map((k) => k + ": " + inspect(v[k], depth + 1)).join(", ") + " }"
        : "{}";
    }
    seen.delete(v);
    return out;
  };
  globalThis.__devil_util = {
    inspect,
    format: (...a) => a.map((x) => (typeof x === "string" ? x : inspect(x))).join(" "),
    isDeepStrictEqual: (a, b) => JSON.stringify(a) === JSON.stringify(b),
    promisify: (fn) => (...a) => new Promise((res, rej) => fn(...a, (e, v) => (e ? rej(e) : res(v)))),
    callbackify: (fn) => (...a) => { const cb = a.pop(); fn(...a).then((v) => cb(null, v), cb); },
    types: { isDate: (v) => v instanceof Date, isRegExp: (v) => v instanceof RegExp },
  };
})();
"#;

const SHIM_EVENTS: &str = r#"
(function () {
  class EventEmitter {
    constructor() { this.__listeners = Object.create(null); }
    on(event, fn) { (this.__listeners[event] || (this.__listeners[event] = [])).push(fn); return this; }
    once(event, fn) {
      const wrapped = (...args) => { this.off(event, wrapped); fn.apply(this, args); };
      return this.on(event, wrapped);
    }
    off(event, fn) {
      const list = this.__listeners[event];
      if (list) this.__listeners[event] = list.filter((f) => f !== fn);
      return this;
    }
    removeListener(event, fn) { return this.off(event, fn); }
    removeAllListeners(event) {
      if (event) delete this.__listeners[event];
      else this.__listeners = Object.create(null);
      return this;
    }
    emit(event, ...args) {
      const list = this.__listeners[event];
      if (!list || list.length === 0) return false;
      for (const fn of list.slice()) fn.apply(this, args);
      return true;
    }
    listeners(event) { return (this.__listeners[event] || []).slice(); }
    listenerCount(event) { return (this.__listeners[event] || []).length; }
    setMaxListeners() { return this; }
  }
  globalThis.__devil_events = { EventEmitter, defaultMaxListeners: 10 };
})();
"#;

const SHIM_URL: &str = r#"
(function () {
  class URLSearchParams {
    constructor(init) {
      this.__pairs = [];
      if (typeof init === "string") {
        for (const part of init.replace(/^\?/, "").split("&")) {
          if (!part) continue;
          const i = part.indexOf("=");
          const k = i < 0 ? part : part.slice(0, i);
          const v = i < 0 ? "" : part.slice(i + 1);
          this.__pairs.push([decodeURIComponent(k.replace(/\+/g, " ")), decodeURIComponent(v.replace(/\+/g, " "))]);
        }
      } else if (Array.isArray(init)) {
        for (const pair of init) this.__pairs.push([String(pair[0]), String(pair[1])]);
      } else if (init && typeof init === "object") {
        for (const k of Object.keys(init)) this.append(k, init[k]);
      }
    }
    append(k, v) { this.__pairs.push([String(k), String(v)]); }
    get(k) { const p = this.__pairs.find(([key]) => key === k); return p ? p[1] : null; }
    getAll(k) { return this.__pairs.filter(([key]) => key === k).map(([, v]) => v); }
    has(k) { return this.__pairs.some(([key]) => key === k); }
    set(k, v) { this.delete(k); this.append(k, v); }
    delete(k) { this.__pairs = this.__pairs.filter(([key]) => key !== k); }
    *keys() { for (const [k] of this.__pairs) yield k; }
    *values() { for (const [, v] of this.__pairs) yield v; }
    *entries() { for (const p of this.__pairs.slice()) yield [p[0], p[1]]; }
    forEach(fn) { for (const [k, v] of this.__pairs.slice()) fn(v, k, this); }
    toString() {
      return this.__pairs.map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v)).join("&");
    }
    sort() { this.__pairs.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)); }
  }

  class URL {
    constructor(input, base) {
      let href = String(input);
      if (base && !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href)) {
        const b = new URL(base);
        href = b.origin + (href.startsWith("/") ? href : "/" + href);
      }
      const m = /^([a-zA-Z][a-zA-Z0-9+.-]*:)?(\/\/[^/?#]*)?([^?#]*)(\?[^#]*)?(#.*)?$/.exec(href) || [];
      this.protocol = m[1] || "";
      this.host = m[2] ? m[2].slice(2) : "";
      this.pathname = m[3] || "/";
      this.search = m[4] || "";
      this.hash = m[5] || "";
      this.hostname = this.host.split(":")[0] || "";
      this.port = this.host.split(":")[1] || "";
      this.origin = this.protocol + "//" + this.host;
      this.searchParams = new URLSearchParams(this.search);
    }
    toString() { return this.protocol + "//" + this.host + this.pathname + this.search + this.hash; }
    toJSON() { return this.toString(); }
  }

  globalThis.URL = URL;
  globalThis.URLSearchParams = URLSearchParams;
  globalThis.__devil_url = { URL, URLSearchParams };
  globalThis.__devil_querystring = {
    parse: (s) => { const o = {}; for (const [k, v] of new URLSearchParams(s).entries()) o[k] = v; return o; },
    stringify: (o) => new URLSearchParams(o).toString(),
  };
})();
"#;

const SHIM_BUFFER: &str = r#"
(function () {
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

  const encodeUtf8 = (str) => {
    const out = [];
    for (let i = 0; i < str.length; i++) {
      let c = str.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff && i + 1 < str.length) {
        const next = str.charCodeAt(i + 1);
        c = 0x10000 + ((c - 0xd800) << 10) + (next - 0xdc00);
        i++;
      }
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
      else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
    }
    return Uint8Array.from(out);
  };

  const decodeUtf8 = (bytes) => {
    let out = "";
    for (let i = 0; i < bytes.length;) {
      const b = bytes[i];
      if (b < 0x80) { out += String.fromCharCode(b); i += 1; }
      else if (b < 0xe0) { out += String.fromCharCode(((b & 0x1f) << 6) | (bytes[i + 1] & 0x3f)); i += 2; }
      else if (b < 0xf0) {
        out += String.fromCharCode(((b & 0x0f) << 12) | ((bytes[i + 1] & 0x3f) << 6) | (bytes[i + 2] & 0x3f));
        i += 3;
      } else {
        const cp = ((b & 0x07) << 18) | ((bytes[i + 1] & 0x3f) << 12) | ((bytes[i + 2] & 0x3f) << 6) | (bytes[i + 3] & 0x3f);
        const off = cp - 0x10000;
        out += String.fromCharCode(0xd800 + (off >> 10), 0xdc00 + (off & 0x3ff));
        i += 4;
      }
    }
    return out;
  };

  globalThis.TextEncoder = class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(input = "") { return encodeUtf8(String(input)); }
  };
  globalThis.TextDecoder = class TextDecoder {
    get encoding() { return "utf-8"; }
    decode(input) { return decodeUtf8(input || new Uint8Array(0)); }
  };

  globalThis.btoa = (input) => {
    const bytes = encodeUtf8(String(input));
    let out = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const a = bytes[i], b = bytes[i + 1], c = bytes[i + 2];
      out += B64[a >> 2]
        + B64[((a & 3) << 4) | ((b || 0) >> 4)]
        + (b === undefined ? "=" : B64[((b & 15) << 2) | ((c || 0) >> 6)])
        + (c === undefined ? "=" : B64[c & 63]);
    }
    return out;
  };
  globalThis.atob = (input) => {
    const clean = String(input).replace(/[^A-Za-z0-9+/]/g, "");
    const bytes = [];
    for (let i = 0; i < clean.length; i += 4) {
      const n = [B64.indexOf(clean[i]), B64.indexOf(clean[i + 1]), B64.indexOf(clean[i + 2]), B64.indexOf(clean[i + 3])];
      bytes.push(((n[0] << 2) | (n[1] >> 4)) & 255);
      if (n[2] >= 0) bytes.push((((n[1] & 15) << 4) | (n[2] >> 2)) & 255);
      if (n[3] >= 0) bytes.push((((n[2] & 3) << 6) | n[3]) & 255);
    }
    return decodeUtf8(Uint8Array.from(bytes));
  };

  const toBytes = (data, encoding) => {
    if (typeof data === "string") {
      return encoding === "base64" ? Uint8Array.from(atob(data), (c) => c.charCodeAt(0)) : encodeUtf8(data);
    }
    if (Array.isArray(data)) return Uint8Array.from(data);
    return Uint8Array.from(data || []);
  };

  globalThis.Buffer = {
    from: toBytes,
    alloc: (n) => new Uint8Array(n),
    isBuffer: (v) => v instanceof Uint8Array,
    concat: (list) => {
      const total = list.reduce((n, b) => n + b.length, 0);
      const out = new Uint8Array(total);
      let off = 0;
      for (const b of list) { out.set(b, off); off += b.length; }
      return out;
    },
    byteLength: (b) => b.length,
    toString: (b, encoding) => (encoding === "base64" ? btoa(decodeUtf8(b)) : decodeUtf8(b)),
  };
  globalThis.__devil_buffer = { encodeUtf8, decodeUtf8 };
})();
"#;

const SHIM_CRYPTO: &str = r#"
(function () {
  const hex = (n) => __devil_host_random_hex(n);
  const uuid = () => {
    const s = hex(16);
    return s.slice(0, 8) + "-" + s.slice(8, 12) + "-4" + s.slice(13, 16)
      + "-a" + s.slice(17, 20) + "-" + s.slice(20, 32);
  };
  globalThis.crypto = {
    randomUUID: uuid,
    getRandomValues: (arr) => { arr.set(Uint8Array.from(hex(arr.length).match(/../g).map((b) => parseInt(b, 16)))); return arr; },
  };
  globalThis.__devil_crypto = {
    randomUUID: uuid,
    randomBytes: (n) => Uint8Array.from(hex(n).match(/../g).map((b) => parseInt(b, 16))),
    createHash: (algo) => {
      if (String(algo).toLowerCase().replace("-", "") !== "sha256") {
        throw new Error("devil runtime implements sha256 only, got " + algo);
      }
      let acc = "";
      return {
        update(data) { acc += String(data); return this; },
        digest(encoding) {
          const out = __devil_host_sha256(acc);
          if (encoding === "base64") return btoa(out);
          if (encoding === "hex" || encoding === undefined) return out;
          return Uint8Array.from(out.match(/../g).map((b) => parseInt(b, 16)));
        },
      };
    },
  };
})();
"#;

const SHIM_TIMERS: &str = r#"
(function () {
  const timers = [];
  let seq = 0;

  const register = (fn, ms, args, every) => {
    const id = ++seq;
    const delay = Number(ms) || 0;
    timers.push({
      id,
      fn: typeof fn === "function" ? fn : new Function(String(fn)),
      args: args || [],
      every: every || 0,
      due: Date.now() + delay,
    });
    return id;
  };

  globalThis.setTimeout = (fn, ms, ...args) => register(fn, ms, args, 0);
  globalThis.setInterval = (fn, ms, ...args) => register(fn, ms, args, Math.max(Number(ms) || 1, 1));
  const cancel = (id) => {
    const i = timers.findIndex((t) => t.id === id);
    if (i >= 0) timers.splice(i, 1);
  };
  globalThis.clearTimeout = cancel;
  globalThis.clearInterval = cancel;

  // Driven from Rust: run everything that is due, then report how long the
  // host should sleep before the next timer (-1 when nothing is pending).
  globalThis.__devil_pump_timers = function () {
    const now = Date.now();
    let next = -1;
    for (const timer of timers.slice()) {
      if (timer.due <= now) {
        try {
          timer.fn.apply(null, timer.args);
        } catch (err) {
          __devil_print_err((err && err.stack) || String(err));
        }
        if (timer.every) {
          timer.due = now + timer.every;
        } else {
          const i = timers.indexOf(timer);
          if (i >= 0) timers.splice(i, 1);
        }
      } else {
        const wait = timer.due - now;
        if (next < 0 || wait < next) next = wait;
      }
    }
    return next;
  };
  globalThis.__devil_has_timers = () => timers.length > 0;
})();
"#;

const SHIM_MISC: &str = r#"
(function () {
  globalThis.performance = globalThis.performance || { now: () => __devil_host_now() };
  globalThis.queueMicrotask = globalThis.queueMicrotask || ((fn) => Promise.resolve().then(fn));
  globalThis.structuredClone = globalThis.structuredClone || ((v) => JSON.parse(JSON.stringify(v)));

  const request = (method, url, headers, body) => {
    let parsed;
    try {
      parsed = __devil_host_http(method, String(url), JSON.stringify(headers || {}), body == null ? "" : String(body));
    } catch (err) {
      return Promise.reject(new Error("fetch failed: " + (err && err.message ? err.message : err)));
    }
    const res = JSON.parse(parsed);
    return Promise.resolve({
      ok: res.status >= 200 && res.status < 300,
      status: res.status,
      statusText: "",
      url: String(url),
      headers: { get: () => null },
      text: () => Promise.resolve(res.body),
      json: () => Promise.resolve(JSON.parse(res.body)),
      arrayBuffer: () => Promise.resolve(new TextEncoder().encode(res.body).buffer),
    });
  };
  globalThis.fetch = (url, options) => request((options && options.method) || "GET", url, options && options.headers, options && options.body);
  globalThis.__devil_http = { request, get: (u, o) => request("GET", u, o && o.headers, o && o.body) };

  globalThis.__devil_assert = (cond, message) => { if (!cond) throw new Error(message || "assertion failed"); };
  globalThis.__devil_zlib = { gzipSync: (s) => s, gunzipSync: (s) => s, deflateSync: (s) => s, inflateSync: (s) => s };
  globalThis.__devil_stream = {
    Readable: class {}, Writable: class {}, Transform: class {}, PassThrough: class {},
  };
  globalThis.__devil_string_decoder = {
    StringDecoder: class { write() { return ""; } end() { return ""; } },
  };
  globalThis.__devil_net = { connect: () => { throw new Error("net is not available in the devil runtime"); } };
  globalThis.__devil_tty = { isTTY: () => false };
  globalThis.__devil_worker_threads = { isMainThread: true };
})();
"#;

/// Republish every shim under the name the bundle loader expects.
const SHIM_TABLE: &str = r#"
(function () {
  const path = globalThis.__devil_path;
  const timers = {
    setTimeout: globalThis.setTimeout,
    clearTimeout: globalThis.clearTimeout,
    setInterval: globalThis.setInterval,
    clearInterval: globalThis.clearInterval,
  };

  const b = {
    console: globalThis.console,
    process: globalThis.process,
    path,
    os: globalThis.__devil_os,
    util: globalThis.__devil_util,
    events: globalThis.__devil_events,
    url: globalThis.__devil_url,
    querystring: globalThis.__devil_querystring,
    buffer: globalThis.Buffer,
    timers,
    "timers/promises": {
      setTimeout: (ms) => new Promise((r) => globalThis.setTimeout(r, ms)),
      setImmediate: (fn) => new Promise((r) => globalThis.setTimeout(r, 0, fn)),
    },
    crypto: globalThis.__devil_crypto,
    assert: {
      ok: globalThis.__devil_assert,
      equal: (a, c, m) => globalThis.__devil_assert(a == c, m),
      strictEqual: (a, c, m) => globalThis.__devil_assert(a === c, m),
      deepEqual: (a, c, m) => globalThis.__devil_assert(JSON.stringify(a) === JSON.stringify(c), m),
    },
    zlib: globalThis.__devil_zlib,
    stream: globalThis.__devil_stream,
    string_decoder: globalThis.__devil_string_decoder,
    http: globalThis.__devil_http,
    https: globalThis.__devil_http,
    net: globalThis.__devil_net,
    tty: globalThis.__devil_tty,
    worker_threads: globalThis.__devil_worker_threads,
    perf_hooks: { performance: globalThis.performance },
    fs: {
      readFileSync: (p) => __devil_host_fs.readFileSync(p),
      writeFileSync: (p, d) => __devil_host_fs.writeFileSync(p, d),
      existsSync: (p) => __devil_host_fs.existsSync(p),
      readdirSync: (p) => __devil_host_fs.readdirSync(p),
      mkdirSync: (p) => __devil_host_fs.mkdirSync(p),
      rmSync: (p, o) => __devil_host_fs.rmSync(p, (o && o.force) || false),
      rm: (p, o, cb) => { try { __devil_host_fs.rmSync(p, (o && o.force) || false); cb && cb(null); } catch (e) { cb && cb(e); } },
      statSync: (p) => JSON.parse(__devil_host_fs.statJson(p)),
      promises: {
        readFile: async (p) => __devil_host_fs.readFileSync(p),
        writeFile: async (p, d) => __devil_host_fs.writeFileSync(p, d),
        readdir: async (p) => __devil_host_fs.readdirSync(p),
        mkdir: async (p) => __devil_host_fs.mkdirSync(p),
        stat: async (p) => JSON.parse(__devil_host_fs.statJson(p)),
      },
    },
  };

  globalThis.__devil_builtins = b;
})();
"#;

/// Globals the CLI sets before evaluating user code.
pub fn prelude(platform: &str, arch: &str) -> String {
    format!(
        "globalThis.__devil_host_platform = {platform:?};\n\
         globalThis.__devil_host_arch = {arch:?};\n"
    )
}

/// Snapshot of `process.env`, serialised for the JavaScript shim.
pub fn env_json() -> String {
    let map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
}
