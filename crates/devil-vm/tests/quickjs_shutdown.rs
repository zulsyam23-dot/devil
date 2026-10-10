use rquickjs::{Context, Runtime};

/// Regression guard: creating a context, evaluating, and dropping everything
/// in the right order must not trip QuickJS's `list_empty` assertion.
///
/// The trap is returning a host `Object` out of a Rust callback: the value is
/// handed to JavaScript and still referenced by the runtime, so it survives the
/// teardown. Host functions therefore return JSON strings and the JavaScript
/// shims parse them.
#[test]
fn plain_runtime_shuts_down_cleanly() {
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    let value: i32 = ctx.with(|c| c.eval("1 + 1")).unwrap();
    assert_eq!(value, 2);
    drop(ctx);
    drop(rt);
}

#[test]
fn host_function_roundtrip() {
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    ctx.with(|c| {
        let f = rquickjs::Function::new(c.clone(), |name: String| format!("hello {name}")).unwrap();
        c.globals().set("greet", f).unwrap();
        let out: String = c.eval("greet('world')").unwrap();
        assert_eq!(out, "hello world");
    });
    drop(ctx);
    drop(rt);
}

#[test]
fn json_returning_function() {
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    ctx.with(|c| {
        let f = rquickjs::Function::new(c.clone(), || r#"{"ok":true}"#.to_string()).unwrap();
        c.globals().set("make", f).unwrap();
        let out: bool = c.eval("JSON.parse(make()).ok").unwrap();
        assert!(out);
    });
    drop(ctx);
    drop(rt);
}

#[test]
fn optional_and_rest_arguments() {
    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    ctx.with(|c| {
        let two = rquickjs::Function::new(c.clone(), |a: String, b: String| {
            format!("{a}-{b}")
        })
        .unwrap();
        c.globals().set("pair", two).unwrap();

        let opt = rquickjs::Function::new(
            c.clone(),
            |name: String, flags: rquickjs::function::Opt<String>| {
                flags.0.unwrap_or_else(|| format!("{name}?")) + "!"
            },
        )
        .unwrap();
        c.globals().set("flag", opt).unwrap();

        let rest =
            rquickjs::Function::new(c.clone(), |items: rquickjs::function::Rest<String>| {
                items.0.into_iter().collect::<Vec<_>>().join(",")
            })
        .unwrap();
        c.globals().set("join", rest).unwrap();

        assert_eq!(c.eval::<String, _>("pair('a','b')").unwrap(), "a-b");
        assert_eq!(c.eval::<String, _>("flag('x')").unwrap(), "x?!");
        assert_eq!(c.eval::<String, _>("flag('x','y')").unwrap(), "y!");
        assert_eq!(c.eval::<String, _>("join('a','b','c')").unwrap(), "a,b,c");
    });
    drop(ctx);
    drop(rt);
}