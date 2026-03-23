//! Evaluator for Glia expressions.
//!
//! Resolution order for list forms:
//! 1. Special forms (`def`, `if`, `do`, `let`, `quote`, `defmacro`, `gensym`) — unevaluated args
//! 2. Macro expansion — if head resolves to `Val::Macro`, expand and re-eval
//! 3. Generic dispatch — eval args, delegate to [`Dispatch`]
//!
//! Non-list values are self-evaluating (returned as-is), except symbols
//! which are looked up in [`Env`] (unbound symbols pass through).
//!
//! Capability dispatch (host, executor, ipfs, etc.) is provided by the
//! caller via the [`Dispatch`] trait — the evaluator itself is host-agnostic.

use core::future::Future;
use core::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Val;

/// Global counter for `gensym` — produces unique symbol names `G__0`, `G__1`, etc.
static GENSYM_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// FnArity — parameter spec for a single arity of a fn/macro
// ---------------------------------------------------------------------------

/// One arity of a function or macro: a parameter list and a body.
///
/// `params` are the fixed positional parameter names.
/// If `variadic` is `Some(name)`, trailing arguments are collected into a
/// list bound to that name (like Clojure's `& rest`).
/// `body` is a sequence of forms evaluated in an implicit `do`.
#[derive(Debug, Clone)]
pub struct FnArity {
    pub params: Vec<String>,
    pub variadic: Option<String>,
    pub body: Vec<Val>,
}

// ---------------------------------------------------------------------------
// Env — lexical scope chain
// ---------------------------------------------------------------------------

/// A lexical environment: a stack of frames where each frame maps names to values.
///
/// Lookup walks from the innermost (last) frame outward.  `push_frame` /
/// `pop_frame` create and destroy child scopes (used by future `let` / `fn`
/// special forms).
#[derive(Debug, Clone)]
pub struct Env {
    frames: Vec<Frame>,
}

impl Default for Env {
    /// Default creates an Env with one root frame (same as `Env::new()`).
    fn default() -> Self {
        Self::new()
    }
}

type Frame = std::collections::HashMap<String, Val>;

impl Env {
    /// Create a new, empty environment with a single root frame.
    pub fn new() -> Self {
        Self {
            frames: vec![Frame::new()],
        }
    }

    /// Look up a binding by name, searching from innermost scope outward.
    pub fn get(&self, name: &str) -> Option<&Val> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.get(name) {
                return Some(v);
            }
        }
        None
    }

    /// Bind `name` to `val` in the innermost (current) frame.
    pub fn set(&mut self, name: String, val: Val) {
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name, val);
        }
    }

    /// Push a new empty child frame (enters a new scope).
    pub fn push_frame(&mut self) {
        self.frames.push(Frame::new());
    }

    /// Pop the innermost frame (exits a scope).  The root frame cannot be popped.
    pub fn pop_frame(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    /// Bind `name` to `val` in the root (outermost) frame.
    /// Used by `def` — definitions are always global, like Clojure's `def`.
    pub fn set_root(&mut self, name: String, val: Val) {
        if let Some(frame) = self.frames.first_mut() {
            frame.insert(name, val);
        }
    }

    /// Collapse all frames into a single merged HashMap (inner overrides outer).
    /// Returns a new Env with one frame containing all visible bindings.
    /// Used by `fn` and `defmacro` to capture the definition-time environment.
    pub fn snapshot(&self) -> Self {
        let mut merged = Frame::new();
        for frame in &self.frames {
            for (k, v) in frame {
                merged.insert(k.clone(), v.clone());
            }
        }
        Self {
            frames: vec![merged],
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch — external command routing
// ---------------------------------------------------------------------------

/// Trait for dispatching evaluated calls to external handlers.
///
/// The kernel (or any host) implements this to route capability calls
/// like `(host id)`, `(ipfs cat ...)`, etc.
pub trait Dispatch {
    /// Invoke the command `name` with already-evaluated `args`.
    fn call<'a>(
        &'a mut self,
        name: &'a str,
        args: &'a [Val],
    ) -> Pin<Box<dyn Future<Output = Result<Val, String>> + 'a>>;
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

/// Returns true if `val` is logically truthy (Clojure model).
/// Only `nil` and `false` are falsy — everything else is truthy,
/// including `0`, empty string, and empty collections.
fn is_truthy(val: &Val) -> bool {
    !matches!(val, Val::Nil | Val::Bool(false))
}

/// Evaluate arguments: recursively evaluate nested lists, look up symbols
/// in env (pass through if unbound), and return non-list/non-sym values as-is.
///
/// Used by the generic dispatch path and future fn invocation.
async fn eval_args<'a, D: Dispatch>(
    raw_args: &'a [Val],
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Result<Vec<Val>, String> {
    let mut args = Vec::with_capacity(raw_args.len());
    for a in raw_args {
        match a {
            Val::List(_) => args.push(eval(a, env, dispatch).await?),
            Val::Sym(s) => match env.get(s) {
                Some(v) => args.push(v.clone()),
                None => args.push(a.clone()),
            },
            other => args.push(other.clone()),
        }
    }
    Ok(args)
}

// ---------------------------------------------------------------------------
// Special forms — each receives RAW (unevaluated) args
// ---------------------------------------------------------------------------

/// `(def name value)` — evaluate value, bind name in root frame.
async fn eval_def<'a, D: Dispatch>(
    args: &'a [Val],
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Result<Val, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(format!(
            "def: expected (def name) or (def name value), got {} args",
            args.len()
        ));
    }
    let name = match &args[0] {
        Val::Sym(s) => s.clone(),
        other => return Err(format!("def: expected symbol, got {other}")),
    };
    let val = match args.get(1) {
        Some(expr) => eval(expr, env, dispatch).await?,
        None => Val::Nil,
    };
    env.set_root(name, val.clone());
    Ok(val)
}

/// `(if test then)` or `(if test then else)` — lazy eval of branches.
async fn eval_if<'a, D: Dispatch>(
    args: &'a [Val],
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Result<Val, String> {
    if args.len() < 2 || args.len() > 3 {
        return Err(format!("if: expected 2-3 args, got {}", args.len()));
    }
    let test_val = eval(&args[0], env, dispatch).await?;
    if is_truthy(&test_val) {
        eval(&args[1], env, dispatch).await
    } else if args.len() == 3 {
        eval(&args[2], env, dispatch).await
    } else {
        Ok(Val::Nil)
    }
}

/// `(do forms...)` — evaluate sequentially, return last.
async fn eval_do<'a, D: Dispatch>(
    args: &'a [Val],
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Result<Val, String> {
    let mut result = Val::Nil;
    for form in args {
        result = eval(form, env, dispatch).await?;
    }
    Ok(result)
}

/// `(let [bindings...] body...)` — local scope with sequential bindings.
async fn eval_let<'a, D: Dispatch>(
    args: &'a [Val],
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Result<Val, String> {
    let bindings = match args.first() {
        Some(Val::Vector(v)) => v,
        Some(other) => return Err(format!("let: expected vector of bindings, got {other}")),
        None => return Err("let: expected (let [bindings...] body...)".into()),
    };
    if bindings.len() % 2 != 0 {
        return Err("let: bindings must be pairs (even number of forms)".into());
    }

    env.push_frame();

    // Evaluate bindings and body in a block so we always pop the frame,
    // even if an eval error occurs mid-binding or mid-body.
    let result = async {
        for pair in bindings.chunks(2) {
            let name = match &pair[0] {
                Val::Sym(s) => s.clone(),
                other => {
                    return Err(format!("let: binding name must be a symbol, got {other}"));
                }
            };
            let val = eval(&pair[1], env, dispatch).await?;
            env.set(name, val);
        }

        // Body forms (implicit do).
        let body = &args[1..];
        let mut result = Val::Nil;
        for form in body {
            result = eval(form, env, dispatch).await?;
        }
        Ok(result)
    }
    .await;

    env.pop_frame();
    result
}

// ---------------------------------------------------------------------------
// Parameter parsing (shared by fn/defmacro)
// ---------------------------------------------------------------------------

/// Parse a parameter vector like `[a b & rest]` into fixed params and optional
/// variadic rest parameter.  Returns `(params, variadic)`.
fn parse_params(param_vec: &[Val]) -> Result<(Vec<String>, Option<String>), String> {
    let mut params = Vec::new();
    let mut variadic = None;
    let mut i = 0;
    while i < param_vec.len() {
        match &param_vec[i] {
            Val::Sym(s) if s == "&" => {
                // Next element must be the rest-param name.
                if i + 1 >= param_vec.len() {
                    return Err("expected symbol after &".into());
                }
                match &param_vec[i + 1] {
                    Val::Sym(rest) => variadic = Some(rest.clone()),
                    other => return Err(format!("expected symbol after &, got {other}")),
                }
                if i + 2 < param_vec.len() {
                    return Err("unexpected forms after & rest parameter".into());
                }
                break;
            }
            Val::Sym(s) => params.push(s.clone()),
            other => return Err(format!("parameter must be a symbol, got {other}")),
        }
        i += 1;
    }
    Ok((params, variadic))
}

// ---------------------------------------------------------------------------
// defmacro
// ---------------------------------------------------------------------------

/// `(defmacro name [params] body...)` or multi-arity `(defmacro name ([p1] b1) ([p2 p3] b2) ...)`
///
/// Like Clojure's defmacro: defines a macro in the root frame.
/// Macros receive unevaluated forms and return a new form to evaluate.
fn eval_defmacro(args: &[Val], env: &mut Env) -> Result<Val, String> {
    if args.is_empty() {
        return Err("defmacro: expected (defmacro name [params] body...)".into());
    }

    let name = match &args[0] {
        Val::Sym(s) => s.clone(),
        other => return Err(format!("defmacro: expected name symbol, got {other}")),
    };

    let rest = &args[1..];
    let arities = parse_arities(rest, "defmacro")?;

    let macro_val = Val::Macro {
        arities,
        env: env.snapshot(),
    };
    env.set_root(name, macro_val.clone());
    Ok(macro_val)
}

/// Parse arity specifications — supports both single-arity and multi-arity forms.
///
/// Single arity: `[params] body...`
/// Multi-arity:  `([p1] body1...) ([p2 p3] body2...)`
fn parse_arities(rest: &[Val], form_name: &str) -> Result<Vec<FnArity>, String> {
    if rest.is_empty() {
        return Err(format!(
            "{form_name}: expected parameter vector or arity list"
        ));
    }

    match &rest[0] {
        // Single arity: [params] body...
        Val::Vector(param_vec) => {
            let (params, variadic) = parse_params(param_vec)?;
            let body = rest[1..].to_vec();
            Ok(vec![FnArity {
                params,
                variadic,
                body,
            }])
        }
        // Multi-arity: (([p1] b1...) ([p2 p3] b2...))
        Val::List(_) => {
            let mut arities = Vec::new();
            for arity_form in rest {
                match arity_form {
                    Val::List(items) if !items.is_empty() => match &items[0] {
                        Val::Vector(param_vec) => {
                            let (params, variadic) = parse_params(param_vec)?;
                            let body = items[1..].to_vec();
                            arities.push(FnArity {
                                params,
                                variadic,
                                body,
                            });
                        }
                        other => {
                            return Err(format!(
                                "{form_name}: expected [params] in arity, got {other}"
                            ));
                        }
                    },
                    other => {
                        return Err(format!("{form_name}: expected arity list, got {other}"));
                    }
                }
            }
            Ok(arities)
        }
        other => Err(format!(
            "{form_name}: expected [params] or arity list, got {other}"
        )),
    }
}

// ---------------------------------------------------------------------------
// Macro invocation
// ---------------------------------------------------------------------------

/// Invoke a macro: bind raw (unevaluated) args to params, evaluate the body
/// in the macro's captured environment, and return the expanded form.
async fn invoke_macro<D: Dispatch>(
    arities: &[FnArity],
    captured_env: &Env,
    raw_args: &[Val],
    dispatch: &mut D,
) -> Result<Val, String> {
    // Find matching arity.
    let arity = find_matching_arity(arities, raw_args.len()).ok_or_else(|| {
        let expected: Vec<String> = arities
            .iter()
            .map(|a| {
                if a.variadic.is_some() {
                    format!("{}+", a.params.len())
                } else {
                    format!("{}", a.params.len())
                }
            })
            .collect();
        format!(
            "macro: wrong number of args ({}), expected [{}]",
            raw_args.len(),
            expected.join(", ")
        )
    })?;

    // Create a new env from the captured snapshot + a fresh frame for params.
    let mut macro_env = captured_env.clone();
    macro_env.push_frame();

    // Bind fixed params to raw (unevaluated) args.
    for (i, param) in arity.params.iter().enumerate() {
        macro_env.set(param.clone(), raw_args[i].clone());
    }

    // Bind variadic rest if present.
    if let Some(ref rest_name) = arity.variadic {
        let rest_args: Vec<Val> = raw_args[arity.params.len()..].to_vec();
        macro_env.set(rest_name.clone(), Val::List(rest_args));
    }

    // Evaluate body forms (implicit do) in the macro env.
    let mut result = Val::Nil;
    for form in &arity.body {
        result = eval(form, &mut macro_env, dispatch).await?;
    }

    Ok(result)
}

/// Find the first arity whose parameter count matches the argument count.
fn find_matching_arity(arities: &[FnArity], arg_count: usize) -> Option<&FnArity> {
    arities.iter().find(|a| {
        if a.variadic.is_some() {
            arg_count >= a.params.len()
        } else {
            arg_count == a.params.len()
        }
    })
}

/// Evaluate a Glia expression.
///
/// Resolution order:
/// 1. Special forms — matched by name, receive unevaluated args
/// 2. Macro expansion — if head resolves to Val::Macro, expand and re-eval
/// 3. Generic path — eval args, delegate to Dispatch (capability calls)
///
/// Non-list values are self-evaluating (returned as-is), except symbols
/// which are looked up in `env` (unbound symbols pass through for Dispatch).
pub fn eval<'a, D: Dispatch>(
    expr: &'a Val,
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Pin<Box<dyn Future<Output = Result<Val, String>> + 'a>> {
    Box::pin(async move {
        match expr {
            Val::List(items) if items.is_empty() => Ok(Val::Nil),
            Val::List(items) => {
                let head = match &items[0] {
                    Val::Sym(s) => s.as_str(),
                    _ => return Err(format!("expected symbol, got {}", items[0])),
                };
                let raw_args = &items[1..];

                // --- Special forms (unevaluated args) ---
                match head {
                    "def" => return eval_def(raw_args, env, dispatch).await,
                    "if" => return eval_if(raw_args, env, dispatch).await,
                    "do" => return eval_do(raw_args, env, dispatch).await,
                    "let" => return eval_let(raw_args, env, dispatch).await,
                    "quote" => {
                        return if raw_args.len() != 1 {
                            Err(format!("quote: expected 1 arg, got {}", raw_args.len()))
                        } else {
                            Ok(raw_args[0].clone())
                        };
                    }
                    "defmacro" => return eval_defmacro(raw_args, env),

                    // Built-in: (gensym) — returns a unique symbol G__N.
                    "gensym" => {
                        if !raw_args.is_empty() {
                            return Err(format!("gensym: expected 0 args, got {}", raw_args.len()));
                        }
                        let n = GENSYM_COUNTER.fetch_add(1, Ordering::Relaxed);
                        return Ok(Val::Sym(format!("G__{n}")));
                    }

                    // Reserved for future special forms (#207, #208).
                    // Return clear errors so they don't fall through to Dispatch.
                    "fn" => return Err("fn: not yet implemented (see #207)".into()),
                    "loop" => return Err("loop: not yet implemented (see #208)".into()),
                    "recur" => return Err("recur: not yet implemented (see #208)".into()),

                    _ => {} // fall through to macro check / generic dispatch
                }

                // --- Macro expansion ---
                // If head resolves to a macro, expand it and re-eval the result.
                if let Some(Val::Macro {
                    arities,
                    env: captured_env,
                }) = env.get(head)
                {
                    let arities = arities.clone();
                    let captured_env = captured_env.clone();
                    let expanded = invoke_macro(&arities, &captured_env, raw_args, dispatch)
                        .await
                        .map_err(|e| format!("macro expansion failed: {e}"))?;
                    return eval(&expanded, env, dispatch).await;
                }

                // --- Generic path: eval args, then dispatch to host ---
                let args = eval_args(raw_args, env, dispatch).await?;
                dispatch.call(head, &args).await
            }
            // Symbol lookup.
            Val::Sym(s) => match env.get(s) {
                Some(v) => Ok(v.clone()),
                None => Ok(Val::Sym(s.clone())),
            },
            // Self-evaluating forms.
            other => Ok(other.clone()),
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial dispatcher that records calls and returns nil.
    struct RecordingDispatch {
        calls: Vec<(String, Vec<Val>)>,
    }

    impl RecordingDispatch {
        fn new() -> Self {
            Self { calls: Vec::new() }
        }
    }

    impl Dispatch for RecordingDispatch {
        fn call<'a>(
            &'a mut self,
            name: &'a str,
            args: &'a [Val],
        ) -> Pin<Box<dyn Future<Output = Result<Val, String>> + 'a>> {
            self.calls.push((name.to_string(), args.to_vec()));
            Box::pin(core::future::ready(Ok(Val::Nil)))
        }
    }

    /// Helper to run an async eval in a blocking context.
    fn eval_blocking(
        expr: &Val,
        env: &mut Env,
        dispatch: &mut RecordingDispatch,
    ) -> Result<Val, String> {
        // We can use a trivial executor since our futures are purely synchronous.
        pollster_eval(eval(expr, env, dispatch))
    }

    /// Minimal single-future poll-to-completion (no tokio needed).
    fn pollster_eval<F: Future<Output = T>, T>(mut fut: F) -> T {
        use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(p: *const ()) -> RawWaker {
                RawWaker::new(p, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(core::ptr::null(), &VTABLE)
        }

        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        // SAFETY: we never move the future after pinning.
        let mut fut = unsafe { Pin::new_unchecked(&mut fut) };
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(val) => return val,
                Poll::Pending => panic!("future returned Pending in synchronous test"),
            }
        }
    }

    // --- Env tests ---

    #[test]
    fn env_get_set() {
        let mut env = Env::new();
        assert!(env.get("x").is_none());
        env.set("x".into(), Val::Int(42));
        assert_eq!(env.get("x"), Some(&Val::Int(42)));
    }

    #[test]
    fn env_child_scope_shadows() {
        let mut env = Env::new();
        env.set("x".into(), Val::Int(1));
        env.push_frame();
        env.set("x".into(), Val::Int(2));
        assert_eq!(env.get("x"), Some(&Val::Int(2)));
        env.pop_frame();
        assert_eq!(env.get("x"), Some(&Val::Int(1)));
    }

    #[test]
    fn env_child_sees_parent() {
        let mut env = Env::new();
        env.set("x".into(), Val::Int(1));
        env.push_frame();
        assert_eq!(env.get("x"), Some(&Val::Int(1)));
        env.pop_frame();
    }

    #[test]
    fn env_pop_root_is_noop() {
        let mut env = Env::new();
        env.set("x".into(), Val::Int(1));
        env.pop_frame(); // should not panic or lose the root
        assert_eq!(env.get("x"), Some(&Val::Int(1)));
    }

    // --- eval tests ---

    #[test]
    fn eval_self_evaluating() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        assert_eq!(
            eval_blocking(&Val::Int(42), &mut env, &mut d),
            Ok(Val::Int(42))
        );
        assert_eq!(
            eval_blocking(&Val::Str("hi".into()), &mut env, &mut d),
            Ok(Val::Str("hi".into()))
        );
        assert_eq!(eval_blocking(&Val::Nil, &mut env, &mut d), Ok(Val::Nil));
        assert_eq!(
            eval_blocking(&Val::Bool(true), &mut env, &mut d),
            Ok(Val::Bool(true))
        );
        assert!(d.calls.is_empty());
    }

    #[test]
    fn eval_symbol_lookup() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        env.set("x".into(), Val::Int(99));

        assert_eq!(
            eval_blocking(&Val::Sym("x".into()), &mut env, &mut d),
            Ok(Val::Int(99))
        );
        // Unbound symbols pass through
        assert_eq!(
            eval_blocking(&Val::Sym("unknown".into()), &mut env, &mut d),
            Ok(Val::Sym("unknown".into()))
        );
    }

    #[test]
    fn eval_empty_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_blocking(&Val::List(vec![]), &mut env, &mut d),
            Ok(Val::Nil)
        );
    }

    #[test]
    fn eval_dispatches_call() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        let expr = Val::List(vec![Val::Sym("host".into()), Val::Sym("id".into())]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert_eq!(result, Ok(Val::Nil));
        assert_eq!(d.calls.len(), 1);
        assert_eq!(d.calls[0].0, "host");
        assert_eq!(d.calls[0].1, vec![Val::Sym("id".into())]);
    }

    #[test]
    fn eval_nested_list_evaluated_first() {
        let mut env = Env::new();

        // A dispatcher that returns Val::Bytes for "ipfs" and Val::Nil for "host".
        struct TestDispatch;
        impl Dispatch for TestDispatch {
            fn call<'a>(
                &'a mut self,
                name: &'a str,
                _args: &'a [Val],
            ) -> Pin<Box<dyn Future<Output = Result<Val, String>> + 'a>> {
                let result = match name {
                    "ipfs" => Ok(Val::Bytes(vec![1, 2, 3])),
                    "host" => Ok(Val::Nil),
                    _ => Err(format!("unknown: {name}")),
                };
                Box::pin(core::future::ready(result))
            }
        }

        let mut d = TestDispatch;
        // (host listen "chess" (ipfs cat "bin/x.wasm"))
        let expr = Val::List(vec![
            Val::Sym("host".into()),
            Val::Sym("listen".into()),
            Val::Str("chess".into()),
            Val::List(vec![
                Val::Sym("ipfs".into()),
                Val::Sym("cat".into()),
                Val::Str("bin/x.wasm".into()),
            ]),
        ]);
        let result = pollster_eval(eval(&expr, &mut env, &mut d));
        assert_eq!(result, Ok(Val::Nil));
    }

    #[test]
    fn eval_non_symbol_head_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Int(42)]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert!(result.is_err());
    }

    // --- Env: set_root + snapshot ---

    #[test]
    fn env_set_root_writes_outermost() {
        let mut env = Env::new();
        env.push_frame();
        env.set_root("x".into(), Val::Int(42));
        env.pop_frame();
        // x should still be visible in the root frame
        assert_eq!(env.get("x"), Some(&Val::Int(42)));
    }

    #[test]
    fn env_snapshot_merges_frames() {
        let mut env = Env::new();
        env.set("x".into(), Val::Int(1));
        env.set("y".into(), Val::Int(2));
        env.push_frame();
        env.set("x".into(), Val::Int(10)); // shadow x
        env.set("z".into(), Val::Int(3));

        let snap = env.snapshot();
        assert_eq!(snap.get("x"), Some(&Val::Int(10))); // inner wins
        assert_eq!(snap.get("y"), Some(&Val::Int(2))); // from outer
        assert_eq!(snap.get("z"), Some(&Val::Int(3))); // from inner
    }

    // --- def ---

    #[test]
    fn def_binds_in_root() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def x 42)
        let expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("x".into()),
            Val::Int(42),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert_eq!(result, Ok(Val::Int(42)));
        assert_eq!(env.get("x"), Some(&Val::Int(42)));
    }

    #[test]
    fn def_evals_value() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def x (do 1 2 3))
        let expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("x".into()),
            Val::List(vec![
                Val::Sym("do".into()),
                Val::Int(1),
                Val::Int(2),
                Val::Int(3),
            ]),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert_eq!(result, Ok(Val::Int(3)));
        assert_eq!(env.get("x"), Some(&Val::Int(3)));
    }

    #[test]
    fn def_non_symbol_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def 42 "oops")
        let expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Int(42),
            Val::Str("oops".into()),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("def"));
    }

    #[test]
    fn def_inside_let_writes_root() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (let [a 1] (def b 2))
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::Vector(vec![Val::Sym("a".into()), Val::Int(1)]),
            Val::List(vec![
                Val::Sym("def".into()),
                Val::Sym("b".into()),
                Val::Int(2),
            ]),
        ]);
        eval_blocking(&expr, &mut env, &mut d).unwrap();
        // b should be visible at root level (not just inside let)
        assert_eq!(env.get("b"), Some(&Val::Int(2)));
    }

    // --- if ---

    #[test]
    fn if_true_branch() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if true "yes" "no")
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Bool(true),
            Val::Str("yes".into()),
            Val::Str("no".into()),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Str("yes".into()))
        );
    }

    #[test]
    fn if_false_branch() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if false "yes" "no")
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Bool(false),
            Val::Str("yes".into()),
            Val::Str("no".into()),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Str("no".into()))
        );
    }

    #[test]
    fn if_nil_is_falsy() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if nil "yes" "no")
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Nil,
            Val::Str("yes".into()),
            Val::Str("no".into()),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Str("no".into()))
        );
    }

    #[test]
    fn if_zero_is_truthy() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if 0 "yes" "no")
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Int(0),
            Val::Str("yes".into()),
            Val::Str("no".into()),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Str("yes".into()))
        );
    }

    #[test]
    fn if_empty_string_truthy() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if "" "yes" "no")
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Str("".into()),
            Val::Str("yes".into()),
            Val::Str("no".into()),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Str("yes".into()))
        );
    }

    #[test]
    fn if_no_else_returns_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if false "yes")
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Bool(false),
            Val::Str("yes".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn if_wrong_arg_count() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if)
        let expr = Val::List(vec![Val::Sym("if".into())]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
        // (if a b c d)
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Bool(true),
            Val::Int(1),
            Val::Int(2),
            Val::Int(3),
        ]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    #[test]
    fn if_only_evals_taken_branch() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (if true (host "taken") (host "not-taken"))
        // Only "taken" branch should dispatch; "not-taken" should NOT.
        let expr = Val::List(vec![
            Val::Sym("if".into()),
            Val::Bool(true),
            Val::List(vec![Val::Sym("host".into()), Val::Str("taken".into())]),
            Val::List(vec![Val::Sym("host".into()), Val::Str("not-taken".into())]),
        ]);
        eval_blocking(&expr, &mut env, &mut d).unwrap();
        assert_eq!(d.calls.len(), 1);
        assert_eq!(d.calls[0].1, vec![Val::Str("taken".into())]);
    }

    // --- do ---

    #[test]
    fn do_returns_last() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (do 1 2 3)
        let expr = Val::List(vec![
            Val::Sym("do".into()),
            Val::Int(1),
            Val::Int(2),
            Val::Int(3),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(3)));
    }

    #[test]
    fn do_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (do)
        let expr = Val::List(vec![Val::Sym("do".into())]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn do_single() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (do 42)
        let expr = Val::List(vec![Val::Sym("do".into()), Val::Int(42)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(42)));
    }

    // --- let ---

    #[test]
    fn let_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (let [x 1] x)
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::Vector(vec![Val::Sym("x".into()), Val::Int(1)]),
            Val::Sym("x".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn let_shadow() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        env.set("x".into(), Val::Int(1));
        // (let [x 2] x)
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::Vector(vec![Val::Sym("x".into()), Val::Int(2)]),
            Val::Sym("x".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(2)));
        // After let, x should be back to 1
        assert_eq!(env.get("x"), Some(&Val::Int(1)));
    }

    #[test]
    fn let_sequential_binding() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (let [x 1 y x] y) — y sees x from earlier binding
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::Vector(vec![
                Val::Sym("x".into()),
                Val::Int(1),
                Val::Sym("y".into()),
                Val::Sym("x".into()),
            ]),
            Val::Sym("y".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn let_implicit_do() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (let [x 1] 10 20 x) — multiple body forms, returns last
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::Vector(vec![Val::Sym("x".into()), Val::Int(1)]),
            Val::Int(10),
            Val::Int(20),
            Val::Sym("x".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn let_odd_bindings_error() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (let [x] x) — odd number of binding forms
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::Vector(vec![Val::Sym("x".into())]),
            Val::Sym("x".into()),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("pairs"));
    }

    #[test]
    fn let_non_vector_error() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (let (x 1) x) — list instead of vector
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::List(vec![Val::Sym("x".into()), Val::Int(1)]),
            Val::Sym("x".into()),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("vector"));
    }

    // --- quote ---

    #[test]
    fn quote_symbol() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        env.set("x".into(), Val::Int(99));
        // (quote x) — should NOT look up x
        let expr = Val::List(vec![Val::Sym("quote".into()), Val::Sym("x".into())]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Sym("x".into()))
        );
    }

    #[test]
    fn quote_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (quote (+ 1 2)) — should NOT evaluate the list
        let inner = Val::List(vec![Val::Sym("+".into()), Val::Int(1), Val::Int(2)]);
        let expr = Val::List(vec![Val::Sym("quote".into()), inner.clone()]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(inner));
        assert!(d.calls.is_empty()); // no dispatch happened
    }

    // --- reserved forms ---

    #[test]
    fn reserved_forms_error_not_dispatch() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        for form in &["fn", "loop", "recur"] {
            let expr = Val::List(vec![Val::Sym(form.to_string())]);
            let result = eval_blocking(&expr, &mut env, &mut d);
            assert!(result.is_err(), "{form} should error, not dispatch");
            assert!(
                result.unwrap_err().contains("not yet implemented"),
                "{form} error should mention 'not yet implemented'"
            );
        }
        assert!(d.calls.is_empty(), "reserved forms should not dispatch");
    }

    // --- defmacro ---

    #[test]
    fn test_defmacro_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        // (defmacro when [test & body]
        //   (quote (if test (do body))))
        // We build a macro that returns: (if <test> (do <body...>))
        // Since we don't have quasiquote, the macro body constructs the list manually.
        //
        // Actually, let's define a simpler macro that just wraps in `do`:
        // (defmacro my-do [& forms] (quote (do forms)))
        //
        // Even simpler: a macro that returns its first arg unevaluated, which
        // is then evaluated by the caller:
        // (defmacro pass-through [x] x)
        let def_expr = Val::List(vec![
            Val::Sym("defmacro".into()),
            Val::Sym("pass-through".into()),
            Val::Vector(vec![Val::Sym("x".into())]),
            Val::Sym("x".into()), // body: just return x (the raw form)
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();

        // Now use it: (pass-through 42) should expand to 42, then eval to 42.
        let use_expr = Val::List(vec![Val::Sym("pass-through".into()), Val::Int(42)]);
        let result = eval_blocking(&use_expr, &mut env, &mut d);
        assert_eq!(result, Ok(Val::Int(42)));
    }

    #[test]
    fn test_defmacro_with_gensym() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        // Define a macro that uses gensym to create a unique let-binding:
        // (defmacro with-binding [val body]
        //   (let [sym (gensym)]
        //     (quote (let [sym val] body))))
        //
        // Actually, without quasiquote we can't easily construct forms referencing
        // gensym'd symbols. Let's test gensym produces unique symbols and that
        // a macro can use them.

        // First, verify gensym works standalone:
        let g1 = Val::List(vec![Val::Sym("gensym".into())]);
        let r1 = eval_blocking(&g1, &mut env, &mut d).unwrap();

        let g2 = Val::List(vec![Val::Sym("gensym".into())]);
        let r2 = eval_blocking(&g2, &mut env, &mut d).unwrap();

        // Both should be symbols with G__ prefix and different.
        match (&r1, &r2) {
            (Val::Sym(s1), Val::Sym(s2)) => {
                assert!(s1.starts_with("G__"), "gensym should start with G__");
                assert!(s2.starts_with("G__"), "gensym should start with G__");
                assert_ne!(s1, s2, "two gensyms should be different");
            }
            _ => panic!("gensym should return symbols, got {r1} and {r2}"),
        }

        // Now define a macro that calls gensym internally:
        // (defmacro gen-sym-val []
        //   (gensym))
        let def_expr = Val::List(vec![
            Val::Sym("defmacro".into()),
            Val::Sym("gen-sym-val".into()),
            Val::Vector(vec![]),
            Val::List(vec![Val::Sym("gensym".into())]),
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();

        // Calling it should produce a gensym'd symbol.
        let use_expr = Val::List(vec![Val::Sym("gen-sym-val".into())]);
        let result = eval_blocking(&use_expr, &mut env, &mut d).unwrap();
        match result {
            Val::Sym(s) => assert!(
                s.starts_with("G__"),
                "macro using gensym should produce G__"
            ),
            other => panic!("expected symbol from macro, got {other}"),
        }
    }

    #[test]
    fn test_macro_expanding_to_macro() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        // Define a "const" macro: (defmacro my-const [name val] ...)
        // that expands to (def name val).
        // The macro body builds a list: we use a simple approach — just return
        // a list with symbols.
        //
        // Without quasiquote, the macro body needs to construct the form.
        // We'll use the pass-through approach: the macro receives raw forms
        // and we need to construct (def name val).
        //
        // Simplest: define a macro that wraps def.
        // (defmacro my-def [name value]
        //   ;; We need to return (def name value) — but we only have quote.
        //   ;; Since name and value are raw forms passed to us, we can't
        //   ;; easily construct this without list-building primitives.
        //
        // Let's test a different scenario: a macro that expands to a form
        // containing another macro call.
        //
        // (defmacro identity [x] x)
        // (defmacro double-identity [x] (quote (identity x)))
        //
        // Wait, (quote (identity x)) returns the literal list (identity x)
        // where x is the symbol x, not the arg value.
        //
        // Let me think about this differently. The simplest macro-producing-macro
        // test: define macro A, define macro B that expands to a call to A.
        // Since B's expansion produces a form starting with A, the evaluator
        // will expand A too.

        // (defmacro always-42 [] 42)
        let def_a = Val::List(vec![
            Val::Sym("defmacro".into()),
            Val::Sym("always-42".into()),
            Val::Vector(vec![]),
            Val::Int(42),
        ]);
        eval_blocking(&def_a, &mut env, &mut d).unwrap();

        // Verify: (always-42) => 42
        let use_a = Val::List(vec![Val::Sym("always-42".into())]);
        assert_eq!(eval_blocking(&use_a, &mut env, &mut d), Ok(Val::Int(42)));

        // (defmacro wrap-always [] (quote (always-42)))
        // This expands to the literal list (always-42), which is then eval'd
        // and triggers the always-42 macro.
        let def_b = Val::List(vec![
            Val::Sym("defmacro".into()),
            Val::Sym("wrap-always".into()),
            Val::Vector(vec![]),
            Val::List(vec![
                Val::Sym("quote".into()),
                Val::List(vec![Val::Sym("always-42".into())]),
            ]),
        ]);
        eval_blocking(&def_b, &mut env, &mut d).unwrap();

        // (wrap-always) => expands to (always-42) => expands to 42
        let use_b = Val::List(vec![Val::Sym("wrap-always".into())]);
        assert_eq!(eval_blocking(&use_b, &mut env, &mut d), Ok(Val::Int(42)));
    }

    #[test]
    fn test_macro_expansion_error() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        // Define a macro whose body errors.
        // (defmacro bad-macro []
        //   (if))  ;; if with no args => error
        let def_expr = Val::List(vec![
            Val::Sym("defmacro".into()),
            Val::Sym("bad-macro".into()),
            Val::Vector(vec![]),
            Val::List(vec![Val::Sym("if".into())]), // if with 0 args => error
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();

        // Calling it should produce "macro expansion failed: ..."
        let use_expr = Val::List(vec![Val::Sym("bad-macro".into())]);
        let result = eval_blocking(&use_expr, &mut env, &mut d);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("macro expansion failed"),
            "error should mention macro expansion"
        );
    }

    #[test]
    fn test_gensym_unique() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        let g1 = eval_blocking(
            &Val::List(vec![Val::Sym("gensym".into())]),
            &mut env,
            &mut d,
        )
        .unwrap();
        let g2 = eval_blocking(
            &Val::List(vec![Val::Sym("gensym".into())]),
            &mut env,
            &mut d,
        )
        .unwrap();

        assert_ne!(g1, g2, "two gensym calls should produce different symbols");
        match (&g1, &g2) {
            (Val::Sym(s1), Val::Sym(s2)) => {
                assert!(s1.starts_with("G__"));
                assert!(s2.starts_with("G__"));
            }
            _ => panic!("gensym should return Sym"),
        }
    }

    #[test]
    fn test_defmacro_receives_unevaluated_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();

        // Bind x to 99 in the env.
        env.set_root("x".into(), Val::Int(99));

        // (defmacro check-raw [form]
        //   (quote form))
        // This macro receives the raw form and quotes it (returns it as-is
        // via the body `(quote form)` — but `form` is a param that gets
        // looked up in the macro env where it's bound to the raw arg).
        //
        // Actually, the body `(quote form)` returns the literal symbol `form`.
        // We need the body to just be `form` to return the raw value,
        // and then we need to quote the whole macro call to prevent eval of
        // the expanded result.
        //
        // Simpler approach: define a macro that returns a quote of its arg.
        // (defmacro raw-to-quote [form]
        //   form)
        // If we call (raw-to-quote (do 1 2 3)), the macro receives
        // (do 1 2 3) as a raw list, returns it, and then eval re-evaluates it.
        // Result = 3.
        //
        // To prove the arg is raw: if we call (raw-to-quote x) where x=99,
        // the macro receives the symbol x (not 99), returns it, and then
        // eval looks up x => 99.
        //
        // To truly test rawness, we need the macro to inspect the form.
        // Let's use a macro that wraps its arg in quote, so the expanded form
        // is (quote <raw-arg>), and we can see the raw form.
        //
        // (defmacro capture [form]
        //   ;; build: (quote <form>)
        //   ;; We receive `form` as a raw Val. To return (quote form),
        //   ;; the macro body needs to construct that list.
        //   ;; Without list-building primitives, this is tricky.
        //
        // Simplest test: macro receives x (symbol), returns x (symbol),
        // which then gets eval'd in caller env to 99. If args were evaluated,
        // the macro would have received 99 (Int), returned 99, and eval of
        // 99 is 99. Same result either way.
        //
        // But we can detect the difference if the arg is a list form.
        // (defmacro return-nil [] nil)
        // (defmacro capture-and-quote [form]
        //   form)
        // (capture-and-quote (quote (do 1 2 3)))
        // If raw: form = (quote (do 1 2 3)), eval returns (do 1 2 3) [a list]
        // If evaluated: form = (do 1 2 3), eval returns 3 [an int]

        // Define the macro:
        let def_expr = Val::List(vec![
            Val::Sym("defmacro".into()),
            Val::Sym("capture-and-quote".into()),
            Val::Vector(vec![Val::Sym("form".into())]),
            Val::Sym("form".into()),
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();

        // (capture-and-quote (quote (do 1 2 3)))
        // The arg is (quote (do 1 2 3)).
        // If macro receives raw: form = (quote (do 1 2 3))
        //   Expanded result = (quote (do 1 2 3))
        //   Eval of that = (do 1 2 3) [the list, not evaluated]
        // If macro received evaluated: form = (do 1 2 3) [result of eval'ing quote]
        //   Expanded result = (do 1 2 3)
        //   Eval of that = 3
        let inner = Val::List(vec![
            Val::Sym("do".into()),
            Val::Int(1),
            Val::Int(2),
            Val::Int(3),
        ]);
        let use_expr = Val::List(vec![
            Val::Sym("capture-and-quote".into()),
            Val::List(vec![Val::Sym("quote".into()), inner.clone()]),
        ]);
        let result = eval_blocking(&use_expr, &mut env, &mut d).unwrap();

        // Should be the list (do 1 2 3), NOT the integer 3.
        assert_eq!(result, inner, "macro should receive unevaluated args");
    }
}
