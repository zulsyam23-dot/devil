//! The `devil run` runtime: QuickJS embedded in the CLI binary.
//!
//! A bundle is executed by a JavaScript engine that ships *inside* `devil`
//! itself, so a program can run on a machine that has never heard of Node.js.
//! QuickJS is a small, fast, embeddable engine; the glue below installs the
//! host objects (`console`, `process`, `fs`, …) that TypeScript code expects.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rquickjs::{CatchResultExt, Context, Runtime, Value};

use devil_core::error::Error;
use devil_core::Result;

pub mod shims;

/// How a program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// The script ran to completion.
    Normal,
    /// `process.exit()` was called.
    Exit,
    /// An uncaught exception.
    Uncaught,
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub kind: ExitKind,
    pub code: i32,
}

impl RunOutcome {
    pub fn exit_code(&self) -> i32 {
        self.code
    }
}

/// Shared state the host functions reach through.
pub struct HostState {
    pub(crate) exit_code: Mutex<Option<i32>>,
    pub(crate) exited: AtomicBool,
    pub(crate) argv: Mutex<Vec<String>>,
    pub(crate) cwd: Mutex<PathBuf>,
    pub(crate) platform: String,
    pub(crate) arch: String,
}

impl HostState {
    fn new(args: Vec<String>, cwd: PathBuf) -> Self {
        HostState {
            exit_code: Mutex::new(None),
            exited: AtomicBool::new(false),
            argv: Mutex::new(args),
            cwd: Mutex::new(cwd),
            platform: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

pub struct VmOptions {
    pub script_name: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub memory_limit: Option<usize>,
    pub stack_size: Option<usize>,
    /// Longest the event loop is allowed to keep waiting for timers.
    pub max_wait: Duration,
}

impl Default for VmOptions {
    fn default() -> Self {
        VmOptions {
            script_name: "<bundle>".to_string(),
            args: Vec::new(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            memory_limit: Some(512 * 1024 * 1024),
            stack_size: Some(4 * 1024 * 1024),
            max_wait: Duration::from_secs(30),
        }
    }
}

/// A QuickJS instance ready to evaluate code.
///
/// Field order matters: the context must be dropped before the runtime, and
/// Rust drops struct fields in declaration order.
pub struct Vm {
    context: Context,
    runtime: Runtime,
    state: Arc<HostState>,
    max_wait: Duration,
}

impl Vm {
    pub fn new(opts: &VmOptions) -> Result<Self> {
        let runtime = Runtime::new().map_err(vm_err)?;
        if let Some(limit) = opts.memory_limit {
            runtime.set_memory_limit(limit);
        }
        if let Some(size) = opts.stack_size {
            runtime.set_max_stack_size(size);
        }

        let context = Context::full(&runtime).map_err(vm_err)?;
        let mut argv = Vec::with_capacity(opts.args.len() + 1);
        argv.push(opts.script_name.clone());
        argv.extend(opts.args.iter().cloned());
        let state = Arc::new(HostState::new(argv, opts.cwd.clone()));

        let vm = Vm {
            context,
            runtime,
            state,
            max_wait: opts.max_wait,
        };

        // Printers first: every shim assumes they exist.
        vm.install_printers();
        vm.context
            .with(|ctx| shims::install(&ctx, &vm.state))
            .map_err(vm_err)?;

        Ok(vm)
    }

    fn install_printers(&self) {
        let outcome: rquickjs::Result<()> = self.context.with(|ctx| {
            let print = rquickjs::Function::new(ctx.clone(), |args: rquickjs::function::Rest<String>| {
                println!("{}", args.iter().cloned().collect::<Vec<_>>().join(" "));
            })?;
            ctx.globals().set("__devil_print", print)?;

            let print_err =
                rquickjs::Function::new(ctx.clone(), |args: rquickjs::function::Rest<String>| {
                    eprintln!("{}", args.iter().cloned().collect::<Vec<_>>().join(" "));
                })?;
            ctx.globals().set("__devil_print_err", print_err)?;
            Ok(())
        });
        if let Err(e) = outcome {
            eprintln!("devil: failed to install console printers: {e}");
        }
    }

    /// Evaluate `source`, then drain microtasks and timers until the program is
    /// idle or `process.exit` fires.
    pub fn run(&self, source: &str) -> Result<RunOutcome> {
        let mut eval_error: Option<String> = None;
        let outcome: Result<(), rquickjs::Error> = self.context.with(|ctx| {
            match ctx.eval::<Value, _>(source).catch(&ctx) {
                Ok(_) => Ok(()),
                Err(err) => {
                    eval_error = Some(format_js_error(&ctx, &err));
                    Ok(())
                }
            }
        });
        if let Err(e) = outcome {
            return Err(vm_err(e));
        }

        if let Some(message) = eval_error {
            self.drain(Duration::from_millis(0));
            if self.exited() {
                return Ok(RunOutcome {
                    kind: ExitKind::Exit,
                    code: self.exit_code(),
                });
            }
            return Err(Error::custom(message));
        }

        self.drain(self.max_wait);

        if self.exited() {
            return Ok(RunOutcome {
                kind: ExitKind::Exit,
                code: self.exit_code(),
            });
        }
        Ok(RunOutcome {
            kind: ExitKind::Normal,
            code: 0,
        })
    }

    /// Run the event loop until nothing is left to do.
    fn drain(&self, budget: Duration) {
        let start = std::time::Instant::now();
        loop {
            let _ = self.runtime.execute_pending_job();

            if self.exited() {
                return;
            }
            if start.elapsed() > budget {
                return;
            }

            let next: Option<f64> = self
                .context
                .with(|ctx| ctx.eval::<Option<f64>, _>("globalThis.__devil_pump_timers && __devil_pump_timers()"))
                .unwrap_or(None);

            match next {
                Some(ms) if ms > 0.0 => {
                    let wait = Duration::from_millis((ms as u64).clamp(1, 50));
                    std::thread::sleep(wait);
                }
                Some(_) => {}
                None => {
                    // No timers: let any pending promise jobs finish, then stop.
                    if !self.runtime.execute_pending_job().unwrap_or(false) {
                        return;
                    }
                }
            }
        }
    }

    /// Evaluate a snippet and return its string form; used by `--eval`.
    pub fn eval_to_string(&self, expression: &str) -> Result<String> {
        self.context.with(|ctx| {
            let wrapped = format!("String((0, eval)({expression}))");
            let value: Value = ctx.eval(wrapped).map_err(vm_err)?;
            value.get::<String>().map_err(vm_err)
        })
    }

    pub fn exit_code(&self) -> i32 {
        self.state
            .exit_code
            .lock()
            .ok()
            .and_then(|c| *c)
            .unwrap_or(0)
    }

    pub fn exited(&self) -> bool {
        self.state.exited.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> &Arc<HostState> {
        &self.state
    }
}

pub(crate) fn vm_err<E: std::fmt::Display>(e: E) -> Error {
    Error::custom(e.to_string())
}

/// Turn a QuickJS exception into a readable message with its stack.
fn format_js_error<'js>(ctx: &rquickjs::Ctx<'js>, err: &rquickjs::CaughtError<'js>) -> String {
    match err {
        rquickjs::CaughtError::Exception(ex) => {
            let message = ex.message().unwrap_or_else(|| "uncaught exception".into());
            match ex.stack() {
                Some(stack) if !stack.is_empty() => format!("{message}\n{stack}"),
                _ => message,
            }
        }
        rquickjs::CaughtError::Value(value) => {
            if let Some(text) = value.as_string() {
                return text.to_string().unwrap_or_else(|_| "thrown string".into());
            }
            let rendered = ctx
                .json_stringify(value.clone())
                .ok()
                .flatten()
                .map(|s| s.to_string().unwrap_or_default())
                .unwrap_or_else(|| format!("{value:?}"));
            format!("uncaught value: {rendered}")
        }
        rquickjs::CaughtError::Error(e) => e.to_string(),
    }
}

/// Evaluate an expression in a throwaway VM, used by `devil eval`.
pub fn eval_expression(source: &str, opts: &VmOptions) -> Result<String> {
    let vm = Vm::new(opts)?;
    vm.eval_to_string(source)
}