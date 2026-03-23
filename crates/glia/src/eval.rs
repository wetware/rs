//! Evaluator for Glia expressions.
//!
//! Resolution order for list forms:
//! 1. Special forms (`def`, `if`, `do`, `let`, `fn`, `quote`, `defmacro`) — unevaluated args
//! 2. Macro expansion — if head resolves to `Val::Macro`, expand with raw args then re-eval
//! 3. Env lookup — if head resolves to `Val::Fn`, invoke the closure
//! 4. Built-in functions (`+`, `list`, `cons`, `apply`, etc.) — eval args, call builtin
//! 5. Generic dispatch — eval args, delegate to [`Dispatch`]
//!
//! Non-list values are self-evaluating (returned as-is), except symbols
//! which are looked up in [`Env`] (unbound symbols pass through).
//!
//! Capability dispatch (host, executor, ipfs, etc.) is provided by the
//! caller via the [`Dispatch`] trait — the evaluator itself is host-agnostic.

use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{FnArity, Val};

/// Monotonic counter for `gensym`.
static GENSYM_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    /// Used by `fn` to capture the definition-time environment.
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

/// Parse a parameter vector into an FnArity.
/// Handles `[x y]` (fixed) and `[x & rest]` (variadic).
fn parse_params(param_vec: &[Val], body: &[Val]) -> Result<FnArity, String> {
    let mut params = Vec::new();
    let mut variadic = None;
    let mut i = 0;
    while i < param_vec.len() {
        match &param_vec[i] {
            Val::Sym(s) if s == "&" => {
                // Next symbol is the variadic rest param
                i += 1;
                match param_vec.get(i) {
                    Some(Val::Sym(rest_name)) => {
                        if variadic.is_some() {
                            return Err("fn: only one & rest param allowed".into());
                        }
                        variadic = Some(rest_name.clone());
                    }
                    _ => return Err("fn: expected symbol after &".into()),
                }
                if i + 1 < param_vec.len() {
                    return Err("fn: nothing allowed after & rest param".into());
                }
            }
            Val::Sym(s) => params.push(s.clone()),
            other => return Err(format!("fn: parameter must be a symbol, got {other}")),
        }
        i += 1;
    }
    Ok(FnArity {
        params,
        variadic,
        body: body.to_vec(),
    })
}

/// `(fn [params] body...)` or `(fn ([params] body...) ([params] body...))` — create a closure.
fn eval_fn(args: &[Val], env: &Env) -> Result<Val, String> {
    if args.is_empty() {
        return Err("fn: expected (fn [params] body...) or (fn ([p] body) ...)".into());
    }

    let arities = match &args[0] {
        // Single-arity: (fn [x y] body...)
        Val::Vector(params) => {
            let arity = parse_params(params, &args[1..])?;
            vec![arity]
        }
        // Multi-arity: (fn ([x] body1) ([x y] body2) ...)
        Val::List(_) => {
            let mut result = Vec::new();
            for arg in args {
                match arg {
                    Val::List(items) if !items.is_empty() => {
                        let param_vec = match &items[0] {
                            Val::Vector(v) => v,
                            other => {
                                return Err(format!(
                                    "fn: multi-arity clause must start with [params], got {other}"
                                ))
                            }
                        };
                        result.push(parse_params(param_vec, &items[1..])?);
                    }
                    other => return Err(format!("fn: expected arity clause (list), got {other}")),
                }
            }
            // Check for overlapping arities (same fixed param count, ignoring variadic)
            let mut seen_counts = std::collections::HashSet::new();
            let mut has_variadic = false;
            for a in &result {
                if a.variadic.is_some() {
                    if has_variadic {
                        return Err("fn: only one variadic arity allowed".into());
                    }
                    has_variadic = true;
                } else if !seen_counts.insert(a.params.len()) {
                    return Err(format!("fn: duplicate arity for {} args", a.params.len()));
                }
            }
            result
        }
        other => {
            return Err(format!(
                "fn: expected [params] or arity clauses, got {other}"
            ))
        }
    };

    Ok(Val::Fn {
        arities,
        env: env.snapshot(),
    })
}

/// Invoke a Val::Fn with evaluated arguments. Matches arity and evaluates body.
async fn invoke_fn<'a, D: Dispatch>(
    arities: &'a [FnArity],
    captured_env: &'a Env,
    args: &[Val],
    dispatch: &'a mut D,
) -> Result<Val, String> {
    // Find matching arity: prefer exact fixed-arity match over variadic.
    // This ensures (fn ([x y] ...) ([x & rest] ...)) called with 2 args
    // picks the fixed 2-arity, not the variadic.
    let arity = arities
        .iter()
        .find(|a| a.variadic.is_none() && args.len() == a.params.len())
        .or_else(|| {
            arities
                .iter()
                .find(|a| a.variadic.is_some() && args.len() >= a.params.len())
        })
        .ok_or_else(|| {
            let expected: Vec<String> = arities
                .iter()
                .map(|a| {
                    if a.variadic.is_some() {
                        format!("{}+", a.params.len())
                    } else {
                        a.params.len().to_string()
                    }
                })
                .collect();
            format!(
                "wrong number of args ({}) passed to fn, expected {}",
                args.len(),
                expected.join(" or ")
            )
        })?;

    // Build fn environment: captured env + new frame with param bindings
    let mut fn_env = captured_env.clone();
    fn_env.push_frame();

    // Bind positional params
    for (name, val) in arity.params.iter().zip(args.iter()) {
        fn_env.set(name.clone(), val.clone());
    }

    // Bind variadic rest param
    if let Some(rest_name) = &arity.variadic {
        let rest_args: Vec<Val> = args[arity.params.len()..].to_vec();
        fn_env.set(rest_name.clone(), Val::List(rest_args));
    }

    // Evaluate body (implicit do)
    let result = async {
        let mut result = Val::Nil;
        for form in &arity.body {
            result = eval(form, &mut fn_env, dispatch).await?;
        }
        Ok(result)
    }
    .await;

    fn_env.pop_frame();
    result
}

/// `(defmacro name [params] body...)` — define a macro in the root frame.
///
/// Like `fn` but the resulting `Val::Macro` receives unevaluated args;
/// the body evaluates in the captured env and the result is re-evaluated
/// in the caller's env.
async fn eval_defmacro(args: &[Val], env: &mut Env) -> Result<Val, String> {
    if args.is_empty() {
        return Err("defmacro: expected (defmacro name [params] body...)".into());
    }
    let name = match &args[0] {
        Val::Sym(s) => s.clone(),
        other => return Err(format!("defmacro: expected symbol for name, got {other}")),
    };
    let fn_args = &args[1..];
    if fn_args.is_empty() {
        return Err("defmacro: expected params after name".into());
    }

    // Reuse the same parsing as eval_fn
    let arities = match &fn_args[0] {
        // Single-arity: (defmacro name [x y] body...)
        Val::Vector(params) => {
            let arity = parse_params(params, &fn_args[1..])?;
            vec![arity]
        }
        // Multi-arity: (defmacro name ([x] body1) ([x y] body2) ...)
        Val::List(_) => {
            let mut result = Vec::new();
            for arg in fn_args {
                match arg {
                    Val::List(items) if !items.is_empty() => {
                        let param_vec = match &items[0] {
                            Val::Vector(v) => v,
                            other => {
                                return Err(format!(
                                    "defmacro: multi-arity clause must start with [params], got {other}"
                                ))
                            }
                        };
                        result.push(parse_params(param_vec, &items[1..])?);
                    }
                    other => {
                        return Err(format!(
                            "defmacro: expected arity clause (list), got {other}"
                        ))
                    }
                }
            }
            // Check for overlapping arities
            let mut seen_counts = std::collections::HashSet::new();
            let mut has_variadic = false;
            for a in &result {
                if a.variadic.is_some() {
                    if has_variadic {
                        return Err("defmacro: only one variadic arity allowed".into());
                    }
                    has_variadic = true;
                } else if !seen_counts.insert(a.params.len()) {
                    return Err(format!(
                        "defmacro: duplicate arity for {} args",
                        a.params.len()
                    ));
                }
            }
            result
        }
        other => {
            return Err(format!(
                "defmacro: expected [params] or arity clauses, got {other}"
            ))
        }
    };

    let val = Val::Macro {
        arities,
        env: env.snapshot(),
    };
    env.set_root(name, val.clone());
    Ok(val)
}

/// Invoke a macro: like invoke_fn but receives raw (unevaluated) args.
/// The macro body evaluates in the captured env; the result is a new form
/// that the caller will re-evaluate in their own env.
async fn invoke_macro<'a, D: Dispatch>(
    arities: &'a [FnArity],
    captured_env: &'a Env,
    raw_args: &[Val],
    dispatch: &'a mut D,
) -> Result<Val, String> {
    // Find matching arity (same logic as invoke_fn)
    let arity = arities
        .iter()
        .find(|a| a.variadic.is_none() && raw_args.len() == a.params.len())
        .or_else(|| {
            arities
                .iter()
                .find(|a| a.variadic.is_some() && raw_args.len() >= a.params.len())
        })
        .ok_or_else(|| {
            let expected: Vec<String> = arities
                .iter()
                .map(|a| {
                    if a.variadic.is_some() {
                        format!("{}+", a.params.len())
                    } else {
                        a.params.len().to_string()
                    }
                })
                .collect();
            format!(
                "wrong number of args ({}) passed to macro, expected {}",
                raw_args.len(),
                expected.join(" or ")
            )
        })?;

    // Build macro environment: captured env + new frame with raw arg bindings
    let mut macro_env = captured_env.clone();
    macro_env.push_frame();

    // Bind positional params to RAW (unevaluated) args
    for (name, val) in arity.params.iter().zip(raw_args.iter()) {
        macro_env.set(name.clone(), val.clone());
    }

    // Bind variadic rest param
    if let Some(rest_name) = &arity.variadic {
        let rest_args: Vec<Val> = raw_args[arity.params.len()..].to_vec();
        macro_env.set(rest_name.clone(), Val::List(rest_args));
    }

    // Evaluate body (implicit do) in the macro's captured env
    let result = async {
        let mut result = Val::Nil;
        for form in &arity.body {
            result = eval(form, &mut macro_env, dispatch).await?;
        }
        Ok(result)
    }
    .await;

    macro_env.pop_frame();
    result
}

/// `(loop [bindings...] body...)` — tail-recursive iteration.
///
/// Bindings are sequential (like `let`).  Body forms are evaluated in
/// an implicit `do`.  If the result is `Val::Recur`, the bindings are
/// replaced and the body re-evaluated; otherwise the result is returned.
async fn eval_loop<'a, D: Dispatch>(
    args: &'a [Val],
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Result<Val, String> {
    let bindings = match args.first() {
        Some(Val::Vector(v)) => v,
        Some(other) => return Err(format!("loop: expected vector of bindings, got {other}")),
        None => return Err("loop: expected (loop [bindings...] body...)".into()),
    };
    if bindings.len() % 2 != 0 {
        return Err("loop: bindings must be pairs (even number of forms)".into());
    }

    let binding_names: Vec<String> = bindings
        .chunks(2)
        .map(|pair| match &pair[0] {
            Val::Sym(s) => Ok(s.clone()),
            other => Err(format!("loop: binding name must be a symbol, got {other}")),
        })
        .collect::<Result<Vec<_>, _>>()?;

    let num_bindings = binding_names.len();

    env.push_frame();

    let result = async {
        // Evaluate initial bindings sequentially (each sees previous ones).
        for pair in bindings.chunks(2) {
            let name = match &pair[0] {
                Val::Sym(s) => s.clone(),
                _ => unreachable!(), // already validated above
            };
            let val = eval(&pair[1], env, dispatch).await?;
            env.set(name, val);
        }

        let body = &args[1..];
        loop {
            // Evaluate body forms (implicit do).
            let mut result = Val::Nil;
            for form in body {
                result = eval(form, env, dispatch).await?;
            }

            match result {
                Val::Recur(new_vals) => {
                    if new_vals.len() != num_bindings {
                        return Err(format!(
                            "recur: expected {} args, got {}",
                            num_bindings,
                            new_vals.len()
                        ));
                    }
                    for (name, val) in binding_names.iter().zip(new_vals) {
                        env.set(name.clone(), val);
                    }
                    // continue loop — re-evaluate body
                }
                other => return Ok(other),
            }
        }
    }
    .await;

    env.pop_frame();
    result
}

/// `(recur args...)` — evaluate args and return a `Recur` sentinel.
///
/// Only meaningful inside `loop` body (tail position).  If it escapes
/// to the top level, `eval_toplevel` converts it to an error.
async fn eval_recur<'a, D: Dispatch>(
    args: &'a [Val],
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Result<Val, String> {
    let evaled = eval_args(args, env, dispatch).await?;
    Ok(Val::Recur(evaled))
}

// ---------------------------------------------------------------------------
// Built-in functions
// ---------------------------------------------------------------------------

/// Check whether `name` is a built-in function. If so, run it on the
/// already-evaluated `args` and return `Some(result)`.
/// Returns `None` if `name` is not a built-in — the caller should fall
/// through to host dispatch.
fn eval_builtin(name: &str, args: &[Val]) -> Option<Result<Val, String>> {
    match name {
        // --- Collections ---
        "list" => Some(Ok(Val::List(args.to_vec()))),
        "cons" => Some(builtin_cons(args)),
        "first" => Some(builtin_first(args)),
        "rest" => Some(builtin_rest(args)),
        "count" => Some(builtin_count(args)),
        "vec" => Some(builtin_vec(args)),
        "get" => Some(builtin_get(args)),
        "assoc" => Some(builtin_assoc(args)),
        "conj" => Some(builtin_conj(args)),
        "concat" => Some(builtin_concat(args)),

        // --- Arithmetic ---
        "+" => Some(builtin_add(args)),
        "-" => Some(builtin_sub(args)),
        "*" => Some(builtin_mul(args)),
        "/" => Some(builtin_div(args)),
        "mod" => Some(builtin_mod(args)),

        // --- Comparison ---
        "=" => Some(builtin_eq(args)),
        "<" => Some(builtin_lt(args)),
        ">" => Some(builtin_gt(args)),
        "<=" => Some(builtin_le(args)),
        ">=" => Some(builtin_ge(args)),

        // --- Other ---
        "gensym" => {
            if !args.is_empty() {
                return Some(Err(format!("gensym: expected 0 args, got {}", args.len())));
            }
            let n = GENSYM_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            Some(Ok(Val::Sym(format!("G__{n}"))))
        }

        _ => None, // not a built-in
    }
}

// --- Collection built-ins ---

fn builtin_cons(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("cons: expected 2 args, got {}", args.len()));
    }
    let tail = match &args[1] {
        Val::List(v) | Val::Vector(v) => v,
        other => {
            return Err(format!(
                "cons: second arg must be List or Vector, got {other}"
            ))
        }
    };
    let mut result = Vec::with_capacity(1 + tail.len());
    result.push(args[0].clone());
    result.extend_from_slice(tail);
    Ok(Val::List(result))
}

fn builtin_first(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("first: expected 1 arg, got {}", args.len()));
    }
    match &args[0] {
        Val::Nil => Ok(Val::Nil),
        Val::List(v) | Val::Vector(v) => Ok(v.first().cloned().unwrap_or(Val::Nil)),
        other => Err(format!("first: expected collection, got {other}")),
    }
}

fn builtin_rest(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("rest: expected 1 arg, got {}", args.len()));
    }
    match &args[0] {
        Val::Nil => Ok(Val::List(vec![])),
        Val::List(v) | Val::Vector(v) => {
            if v.is_empty() {
                Ok(Val::List(vec![]))
            } else {
                Ok(Val::List(v[1..].to_vec()))
            }
        }
        other => Err(format!("rest: expected collection, got {other}")),
    }
}

fn builtin_count(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("count: expected 1 arg, got {}", args.len()));
    }
    let n = match &args[0] {
        Val::Nil => 0,
        Val::List(v) | Val::Vector(v) | Val::Set(v) => v.len(),
        Val::Map(pairs) => pairs.len(),
        Val::Str(s) => s.chars().count(),
        other => return Err(format!("count: expected collection or nil, got {other}")),
    };
    Ok(Val::Int(n as i64))
}

fn builtin_vec(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("vec: expected 1 arg, got {}", args.len()));
    }
    match &args[0] {
        Val::Nil => Ok(Val::Vector(vec![])),
        Val::List(v) => Ok(Val::Vector(v.clone())),
        Val::Vector(_) => Ok(args[0].clone()),
        other => Err(format!("vec: expected list or vector, got {other}")),
    }
}

fn builtin_get(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("get: expected 2 args, got {}", args.len()));
    }
    match &args[0] {
        Val::Map(pairs) => {
            for (k, v) in pairs {
                if k == &args[1] {
                    return Ok(v.clone());
                }
            }
            Ok(Val::Nil)
        }
        Val::Vector(v) => match &args[1] {
            Val::Int(i) => {
                if *i < 0 {
                    Ok(Val::Nil)
                } else {
                    Ok(v.get(*i as usize).cloned().unwrap_or(Val::Nil))
                }
            }
            other => Err(format!("get: vector index must be Int, got {other}")),
        },
        Val::Nil => Ok(Val::Nil),
        other => Err(format!("get: expected map or vector, got {other}")),
    }
}

fn builtin_assoc(args: &[Val]) -> Result<Val, String> {
    if args.is_empty() || !(args.len() - 1).is_multiple_of(2) {
        return Err(format!(
            "assoc: expected map + key-value pairs (odd number of args), got {}",
            args.len()
        ));
    }
    let mut pairs = match &args[0] {
        Val::Map(pairs) => pairs.clone(),
        other => return Err(format!("assoc: first arg must be a map, got {other}")),
    };
    for chunk in args[1..].chunks(2) {
        let key = &chunk[0];
        let val = &chunk[1];
        // Update existing key or append.
        if let Some(entry) = pairs.iter_mut().find(|(k, _)| k == key) {
            entry.1 = val.clone();
        } else {
            pairs.push((key.clone(), val.clone()));
        }
    }
    Ok(Val::Map(pairs))
}

fn builtin_conj(args: &[Val]) -> Result<Val, String> {
    if args.len() < 2 {
        return Err(format!(
            "conj: expected at least 2 args, got {}",
            args.len()
        ));
    }
    match &args[0] {
        Val::Vector(v) => {
            let mut result = v.clone();
            result.extend_from_slice(&args[1..]);
            Ok(Val::Vector(result))
        }
        Val::List(v) => {
            // Clojure: conj on lists PREPENDS each item
            let mut result = v.clone();
            for item in &args[1..] {
                result.insert(0, item.clone());
            }
            Ok(Val::List(result))
        }
        Val::Map(pairs) => {
            let mut result = pairs.clone();
            for item in &args[1..] {
                match item {
                    Val::Vector(pair) if pair.len() == 2 => {
                        if let Some(entry) = result.iter_mut().find(|(k, _)| k == &pair[0]) {
                            entry.1 = pair[1].clone();
                        } else {
                            result.push((pair[0].clone(), pair[1].clone()));
                        }
                    }
                    other => {
                        return Err(format!(
                            "conj: map entries must be [key val] vectors, got {other}"
                        ))
                    }
                }
            }
            Ok(Val::Map(result))
        }
        other => Err(format!("conj: expected collection, got {other}")),
    }
}

fn builtin_concat(args: &[Val]) -> Result<Val, String> {
    let mut result = Vec::new();
    for arg in args {
        match arg {
            Val::Nil => {}
            Val::List(v) | Val::Vector(v) => result.extend(v.iter().cloned()),
            other => return Err(format!("concat: expected sequence or nil, got {other}")),
        }
    }
    Ok(Val::List(result))
}

// --- Arithmetic helpers ---

/// Extract a numeric pair, promoting to Float if mixed.
enum NumPair {
    Ints(i64, i64),
    Floats(f64, f64),
}

fn num_pair(a: &Val, b: &Val) -> Result<NumPair, String> {
    match (a, b) {
        (Val::Int(x), Val::Int(y)) => Ok(NumPair::Ints(*x, *y)),
        (Val::Float(x), Val::Float(y)) => Ok(NumPair::Floats(*x, *y)),
        (Val::Int(x), Val::Float(y)) => Ok(NumPair::Floats(*x as f64, *y)),
        (Val::Float(x), Val::Int(y)) => Ok(NumPair::Floats(*x, *y as f64)),
        _ => Err(format!("expected numbers, got {a} and {b}")),
    }
}

fn builtin_add(args: &[Val]) -> Result<Val, String> {
    let mut acc = Val::Int(0);
    for a in args {
        acc = match num_pair(&acc, a)? {
            NumPair::Ints(x, y) => Val::Int(x + y),
            NumPair::Floats(x, y) => Val::Float(x + y),
        };
    }
    Ok(acc)
}

fn builtin_sub(args: &[Val]) -> Result<Val, String> {
    if args.is_empty() {
        return Err("-: expected at least 1 arg".into());
    }
    if args.len() == 1 {
        return match &args[0] {
            Val::Int(n) => Ok(Val::Int(-n)),
            Val::Float(n) => Ok(Val::Float(-n)),
            other => Err(format!("-: expected number, got {other}")),
        };
    }
    let mut acc = args[0].clone();
    for a in &args[1..] {
        acc = match num_pair(&acc, a)? {
            NumPair::Ints(x, y) => Val::Int(x - y),
            NumPair::Floats(x, y) => Val::Float(x - y),
        };
    }
    Ok(acc)
}

fn builtin_mul(args: &[Val]) -> Result<Val, String> {
    let mut acc = Val::Int(1);
    for a in args {
        acc = match num_pair(&acc, a)? {
            NumPair::Ints(x, y) => Val::Int(x * y),
            NumPair::Floats(x, y) => Val::Float(x * y),
        };
    }
    Ok(acc)
}

fn builtin_div(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("/: expected 2 args, got {}", args.len()));
    }
    match num_pair(&args[0], &args[1])? {
        NumPair::Ints(_, 0) => Err("division by zero".into()),
        NumPair::Ints(x, y) => Ok(Val::Int(x / y)),
        NumPair::Floats(_, 0.0) => Err("division by zero".into()),
        NumPair::Floats(x, y) => Ok(Val::Float(x / y)),
    }
}

fn builtin_mod(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("mod: expected 2 args, got {}", args.len()));
    }
    match num_pair(&args[0], &args[1])? {
        NumPair::Ints(_, 0) => Err("mod: division by zero".into()),
        NumPair::Ints(x, y) => Ok(Val::Int(x % y)),
        NumPair::Floats(_, 0.0) => Err("mod: division by zero".into()),
        NumPair::Floats(x, y) => Ok(Val::Float(x % y)),
    }
}

// --- Comparison built-ins ---

fn builtin_eq(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("=: expected 2 args, got {}", args.len()));
    }
    Ok(Val::Bool(args[0] == args[1]))
}

fn numeric_cmp(a: &Val, b: &Val) -> Result<std::cmp::Ordering, String> {
    match (a, b) {
        (Val::Int(x), Val::Int(y)) => Ok(x.cmp(y)),
        (Val::Float(x), Val::Float(y)) => x
            .partial_cmp(y)
            .ok_or_else(|| "comparison failed (NaN)".to_string()),
        (Val::Int(x), Val::Float(y)) => (*x as f64)
            .partial_cmp(y)
            .ok_or_else(|| "comparison failed (NaN)".to_string()),
        (Val::Float(x), Val::Int(y)) => x
            .partial_cmp(&(*y as f64))
            .ok_or_else(|| "comparison failed (NaN)".to_string()),
        _ => Err(format!("comparison requires numbers, got {a} and {b}")),
    }
}

fn builtin_lt(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("<: expected 2 args, got {}", args.len()));
    }
    Ok(Val::Bool(numeric_cmp(&args[0], &args[1])?.is_lt()))
}

fn builtin_gt(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!(">: expected 2 args, got {}", args.len()));
    }
    Ok(Val::Bool(numeric_cmp(&args[0], &args[1])?.is_gt()))
}

fn builtin_le(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("<=: expected 2 args, got {}", args.len()));
    }
    Ok(Val::Bool(!numeric_cmp(&args[0], &args[1])?.is_gt()))
}

fn builtin_ge(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!(">=: expected 2 args, got {}", args.len()));
    }
    Ok(Val::Bool(!numeric_cmp(&args[0], &args[1])?.is_lt()))
}

/// Top-level evaluation wrapper.
///
/// Calls `eval` and catches escaped `Val::Recur` sentinels, converting
/// them to an error ("recur not in tail position").
pub fn eval_toplevel<'a, D: Dispatch>(
    expr: &'a Val,
    env: &'a mut Env,
    dispatch: &'a mut D,
) -> Pin<Box<dyn Future<Output = Result<Val, String>> + 'a>> {
    Box::pin(async move {
        let result = eval(expr, env, dispatch).await?;
        match result {
            Val::Recur(_) => Err("recur not in tail position".into()),
            other => Ok(other),
        }
    })
}

/// Evaluate a Glia expression.
///
/// Resolution order:
/// 1. Special forms — matched by name, receive unevaluated args
/// 2. Macro expansion — if head is Val::Macro in env, expand + re-eval
/// 3. Env lookup — if head resolves to Val::Fn, invoke it
/// 4. Built-in functions — eval args, call builtin
/// 5. `apply` — special handling (re-dispatches)
/// 6. Generic path — eval args, delegate to Dispatch (capability calls)
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

                    "fn" => return eval_fn(raw_args, env),

                    "loop" => return eval_loop(raw_args, env, dispatch).await,
                    "recur" => return eval_recur(raw_args, env, dispatch).await,

                    "defmacro" => return eval_defmacro(raw_args, env).await,

                    // Reader markers — error if they escape syntax-quote
                    "unquote" => {
                        return Err("unquote (~) not inside syntax-quote".into());
                    }
                    "splice-unquote" => {
                        return Err("splice-unquote (~@) not inside syntax-quote".into());
                    }

                    _ => {} // fall through to macro / fn / builtins / dispatch
                }

                // --- Macro expansion: if head resolves to a macro, expand + eval ---
                if let Some(Val::Macro {
                    arities,
                    env: captured_env,
                }) = env.get(head)
                {
                    let arities = arities.clone();
                    let captured_env = captured_env.clone();
                    // Macro receives RAW (unevaluated) args, body runs in captured env
                    let expanded =
                        invoke_macro(&arities, &captured_env, raw_args, dispatch).await?;
                    // Re-evaluate the expanded form in the CALLER's env
                    return eval(&expanded, env, dispatch).await;
                }

                // --- Env lookup: if head resolves to a fn, invoke it ---
                if let Some(Val::Fn {
                    arities,
                    env: captured_env,
                }) = env.get(head)
                {
                    let arities = arities.clone();
                    let captured_env = captured_env.clone();
                    let args = eval_args(raw_args, env, dispatch).await?;
                    return invoke_fn(&arities, &captured_env, &args, dispatch).await;
                }

                // --- Built-in: apply (needs re-dispatch, so handled here) ---
                if head == "apply" {
                    let args = eval_args(raw_args, env, dispatch).await?;
                    if args.len() < 2 {
                        return Err(format!(
                            "apply: expected at least 2 args, got {}",
                            args.len()
                        ));
                    }
                    // First arg is the function (symbol or Val::Fn)
                    let func = &args[0];
                    // Last arg must be a collection; middle args are prepended
                    let last = &args[args.len() - 1];
                    let trailing = match last {
                        Val::List(v) | Val::Vector(v) => v.clone(),
                        other => {
                            return Err(format!(
                                "apply: last arg must be List or Vector, got {other}"
                            ))
                        }
                    };
                    let mut spread = args[1..args.len() - 1].to_vec();
                    spread.extend(trailing);

                    // Re-dispatch: if func is a symbol, check env for Val::Fn first,
                    // then try builtins, then dispatch.
                    match func {
                        Val::Sym(fname) => {
                            if let Some(Val::Fn {
                                arities,
                                env: captured_env,
                            }) = env.get(fname)
                            {
                                let arities = arities.clone();
                                let captured_env = captured_env.clone();
                                return invoke_fn(&arities, &captured_env, &spread, dispatch).await;
                            }
                            if let Some(result) = eval_builtin(fname, &spread) {
                                return result;
                            }
                            return dispatch.call(fname, &spread).await;
                        }
                        Val::Fn {
                            arities,
                            env: captured_env,
                        } => {
                            let arities = arities.clone();
                            let captured_env = captured_env.clone();
                            return invoke_fn(&arities, &captured_env, &spread, dispatch).await;
                        }
                        other => {
                            return Err(format!(
                                "apply: first arg must be a symbol or fn, got {other}"
                            ))
                        }
                    }
                }

                // --- Built-in functions ---
                let args = eval_args(raw_args, env, dispatch).await?;
                if let Some(result) = eval_builtin(head, &args) {
                    return result;
                }

                // --- Generic path: eval args, then dispatch to host ---
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
        pollster_eval(eval_toplevel(expr, env, dispatch))
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

    // --- fn ---

    #[test]
    fn fn_single_arity_call() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def f (fn [x] x))
        let def_expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("f".into()),
            Val::List(vec![
                Val::Sym("fn".into()),
                Val::Vector(vec![Val::Sym("x".into())]),
                Val::Sym("x".into()),
            ]),
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();
        // (f 42)
        let call_expr = Val::List(vec![Val::Sym("f".into()), Val::Int(42)]);
        let result = eval_blocking(&call_expr, &mut env, &mut d);
        assert_eq!(result, Ok(Val::Int(42)));
    }

    #[test]
    fn fn_multi_arity() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def f (fn ([x] x) ([x y] y)))
        let def_expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("f".into()),
            Val::List(vec![
                Val::Sym("fn".into()),
                Val::List(vec![
                    Val::Vector(vec![Val::Sym("x".into())]),
                    Val::Sym("x".into()),
                ]),
                Val::List(vec![
                    Val::Vector(vec![Val::Sym("x".into()), Val::Sym("y".into())]),
                    Val::Sym("y".into()),
                ]),
            ]),
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();
        // (f 1) → 1
        let call1 = Val::List(vec![Val::Sym("f".into()), Val::Int(1)]);
        assert_eq!(eval_blocking(&call1, &mut env, &mut d), Ok(Val::Int(1)));
        // (f 1 2) → 2
        let call2 = Val::List(vec![Val::Sym("f".into()), Val::Int(1), Val::Int(2)]);
        assert_eq!(eval_blocking(&call2, &mut env, &mut d), Ok(Val::Int(2)));
    }

    #[test]
    fn fn_variadic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def f (fn [x & rest] rest))
        let def_expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("f".into()),
            Val::List(vec![
                Val::Sym("fn".into()),
                Val::Vector(vec![
                    Val::Sym("x".into()),
                    Val::Sym("&".into()),
                    Val::Sym("rest".into()),
                ]),
                Val::Sym("rest".into()),
            ]),
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();
        // (f 1 2 3) → (2 3)
        let call = Val::List(vec![
            Val::Sym("f".into()),
            Val::Int(1),
            Val::Int(2),
            Val::Int(3),
        ]);
        assert_eq!(
            eval_blocking(&call, &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn fn_closure_captures_env() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def x 10)
        let def_x = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("x".into()),
            Val::Int(10),
        ]);
        eval_blocking(&def_x, &mut env, &mut d).unwrap();
        // (def f (fn [] x))
        let def_f = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("f".into()),
            Val::List(vec![
                Val::Sym("fn".into()),
                Val::Vector(vec![]),
                Val::Sym("x".into()),
            ]),
        ]);
        eval_blocking(&def_f, &mut env, &mut d).unwrap();
        // (def x 20) — rebind x
        let def_x2 = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("x".into()),
            Val::Int(20),
        ]);
        eval_blocking(&def_x2, &mut env, &mut d).unwrap();
        // (f) → 10, not 20 (captured at definition time)
        let call = Val::List(vec![Val::Sym("f".into())]);
        assert_eq!(eval_blocking(&call, &mut env, &mut d), Ok(Val::Int(10)));
    }

    #[test]
    fn fn_arity_mismatch() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def f (fn [x y] x))
        let def_expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("f".into()),
            Val::List(vec![
                Val::Sym("fn".into()),
                Val::Vector(vec![Val::Sym("x".into()), Val::Sym("y".into())]),
                Val::Sym("x".into()),
            ]),
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();
        // (f 1) — wrong arity
        let call = Val::List(vec![Val::Sym("f".into()), Val::Int(1)]);
        let err = eval_blocking(&call, &mut env, &mut d).unwrap_err();
        assert!(err.contains("wrong number of args"), "got: {err}");
    }

    #[test]
    fn fn_duplicate_arity_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (fn ([x] x) ([y] y)) — two 1-arg arities
        let expr = Val::List(vec![
            Val::Sym("fn".into()),
            Val::List(vec![
                Val::Vector(vec![Val::Sym("x".into())]),
                Val::Sym("x".into()),
            ]),
            Val::List(vec![
                Val::Vector(vec![Val::Sym("y".into())]),
                Val::Sym("y".into()),
            ]),
        ]);
        let err = eval_blocking(&expr, &mut env, &mut d).unwrap_err();
        assert!(err.contains("duplicate arity"), "got: {err}");
    }

    #[test]
    fn fn_implicit_do_body() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (def f (fn [x] 1 2 x)) — body has multiple forms, returns last
        let def_expr = Val::List(vec![
            Val::Sym("def".into()),
            Val::Sym("f".into()),
            Val::List(vec![
                Val::Sym("fn".into()),
                Val::Vector(vec![Val::Sym("x".into())]),
                Val::Int(1),
                Val::Int(2),
                Val::Sym("x".into()),
            ]),
        ]);
        eval_blocking(&def_expr, &mut env, &mut d).unwrap();
        let call = Val::List(vec![Val::Sym("f".into()), Val::Int(99)]);
        assert_eq!(eval_blocking(&call, &mut env, &mut d), Ok(Val::Int(99)));
    }

    #[test]
    fn fn_no_params_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (fn) — no params at all
        let expr = Val::List(vec![Val::Sym("fn".into())]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- loop / recur ---

    #[test]
    fn loop_returns_non_recur() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (loop [x 42] x)
        let expr = Val::List(vec![
            Val::Sym("loop".into()),
            Val::Vector(vec![Val::Sym("x".into()), Val::Int(42)]),
            Val::Sym("x".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(42)));
    }

    #[test]
    fn loop_recur_once() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (loop [x true] (if x (recur false) "done"))
        let expr = Val::List(vec![
            Val::Sym("loop".into()),
            Val::Vector(vec![Val::Sym("x".into()), Val::Bool(true)]),
            Val::List(vec![
                Val::Sym("if".into()),
                Val::Sym("x".into()),
                Val::List(vec![Val::Sym("recur".into()), Val::Bool(false)]),
                Val::Str("done".into()),
            ]),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Str("done".into()))
        );
    }

    #[test]
    fn loop_recur_multiple_bindings() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (loop [a 1 b 2] (if a (recur false 3) b))
        let expr = Val::List(vec![
            Val::Sym("loop".into()),
            Val::Vector(vec![
                Val::Sym("a".into()),
                Val::Int(1),
                Val::Sym("b".into()),
                Val::Int(2),
            ]),
            Val::List(vec![
                Val::Sym("if".into()),
                Val::Sym("a".into()),
                Val::List(vec![
                    Val::Sym("recur".into()),
                    Val::Bool(false),
                    Val::Int(3),
                ]),
                Val::Sym("b".into()),
            ]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(3)));
    }

    #[test]
    fn loop_sequential_bindings() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (loop [a 1 b a] b) — b sees a=1
        let expr = Val::List(vec![
            Val::Sym("loop".into()),
            Val::Vector(vec![
                Val::Sym("a".into()),
                Val::Int(1),
                Val::Sym("b".into()),
                Val::Sym("a".into()),
            ]),
            Val::Sym("b".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn recur_wrong_arity() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (loop [x 1 y 2] (recur 3))
        let expr = Val::List(vec![
            Val::Sym("loop".into()),
            Val::Vector(vec![
                Val::Sym("x".into()),
                Val::Int(1),
                Val::Sym("y".into()),
                Val::Int(2),
            ]),
            Val::List(vec![Val::Sym("recur".into()), Val::Int(3)]),
        ]);
        let err = eval_blocking(&expr, &mut env, &mut d).unwrap_err();
        assert!(err.contains("expected 2 args"), "got: {err}");
    }

    #[test]
    fn recur_outside_loop() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (recur 1) at top level
        let expr = Val::List(vec![Val::Sym("recur".into()), Val::Int(1)]);
        let err = eval_blocking(&expr, &mut env, &mut d).unwrap_err();
        assert!(err.contains("recur not in tail position"), "got: {err}");
    }

    #[test]
    fn loop_non_vector_bindings() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (loop (x 1) x) — list instead of vector
        let expr = Val::List(vec![
            Val::Sym("loop".into()),
            Val::List(vec![Val::Sym("x".into()), Val::Int(1)]),
            Val::Sym("x".into()),
        ]);
        let err = eval_blocking(&expr, &mut env, &mut d).unwrap_err();
        assert!(err.contains("vector"), "got: {err}");
    }

    #[test]
    fn loop_odd_bindings() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (loop [x] x) — odd number of binding forms
        let expr = Val::List(vec![
            Val::Sym("loop".into()),
            Val::Vector(vec![Val::Sym("x".into())]),
            Val::Sym("x".into()),
        ]);
        let err = eval_blocking(&expr, &mut env, &mut d).unwrap_err();
        assert!(err.contains("pairs"), "got: {err}");
    }

    // =========================================================================
    // Built-in function tests
    // =========================================================================

    /// Helper: parse + eval a string expression.
    fn eval_str(input: &str, env: &mut Env, d: &mut RecordingDispatch) -> Result<Val, String> {
        let expr = crate::read(input).map_err(|e| format!("parse error: {e}"))?;
        eval_blocking(&expr, env, d)
    }

    // --- list ---

    #[test]
    fn builtin_list_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(list)", &mut env, &mut d), Ok(Val::List(vec![])));
    }

    #[test]
    fn builtin_list_with_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(list 1 2 3)", &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    // --- cons ---

    #[test]
    fn builtin_cons_onto_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(cons 1 (list 2 3))", &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_cons_wrong_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(cons 1)", &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_cons_non_collection() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(cons 1 2)", &mut env, &mut d).is_err());
    }

    // --- first ---

    #[test]
    fn builtin_first_of_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(first (list 1 2 3))", &mut env, &mut d),
            Ok(Val::Int(1))
        );
    }

    #[test]
    fn builtin_first_of_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(first (list))", &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn builtin_first_of_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(first nil)", &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn builtin_first_wrong_type() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(first 42)", &mut env, &mut d).is_err());
    }

    // --- rest ---

    #[test]
    fn builtin_rest_of_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(rest (list 1 2 3))", &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_rest_of_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(rest (list))", &mut env, &mut d),
            Ok(Val::List(vec![]))
        );
    }

    #[test]
    fn builtin_rest_of_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(rest nil)", &mut env, &mut d),
            Ok(Val::List(vec![]))
        );
    }

    #[test]
    fn builtin_rest_wrong_type() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(rest 42)", &mut env, &mut d).is_err());
    }

    // --- count ---

    #[test]
    fn builtin_count_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(count (list 1 2 3))", &mut env, &mut d),
            Ok(Val::Int(3))
        );
    }

    #[test]
    fn builtin_count_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(count nil)", &mut env, &mut d), Ok(Val::Int(0)));
    }

    #[test]
    fn builtin_count_string_chars() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Unicode: each emoji is one char
        assert_eq!(
            eval_str(r#"(count "hello")"#, &mut env, &mut d),
            Ok(Val::Int(5))
        );
    }

    #[test]
    fn builtin_count_wrong_type() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(count 42)", &mut env, &mut d).is_err());
    }

    // --- vec ---

    #[test]
    fn builtin_vec_from_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(vec (list 1 2))", &mut env, &mut d),
            Ok(Val::Vector(vec![Val::Int(1), Val::Int(2)]))
        );
    }

    #[test]
    fn builtin_vec_from_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(vec nil)", &mut env, &mut d),
            Ok(Val::Vector(vec![]))
        );
    }

    #[test]
    fn builtin_vec_wrong_type() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(vec 42)", &mut env, &mut d).is_err());
    }

    // --- get ---

    #[test]
    fn builtin_get_map() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(get {:a 1 :b 2} :b)", &mut env, &mut d),
            Ok(Val::Int(2))
        );
    }

    #[test]
    fn builtin_get_map_missing() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(get {:a 1} :z)", &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn builtin_get_vector() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(get [10 20 30] 1)", &mut env, &mut d),
            Ok(Val::Int(20))
        );
    }

    #[test]
    fn builtin_get_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(get nil :a)", &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn builtin_get_wrong_type() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(get 42 0)", &mut env, &mut d).is_err());
    }

    // --- assoc ---

    #[test]
    fn builtin_assoc_add_key() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(assoc {:a 1} :b 2)", &mut env, &mut d),
            Ok(Val::Map(vec![
                (Val::Keyword("a".into()), Val::Int(1)),
                (Val::Keyword("b".into()), Val::Int(2)),
            ]))
        );
    }

    #[test]
    fn builtin_assoc_update_key() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(assoc {:a 1} :a 99)", &mut env, &mut d),
            Ok(Val::Map(vec![(Val::Keyword("a".into()), Val::Int(99))]))
        );
    }

    #[test]
    fn builtin_assoc_wrong_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Even number of args (map + 1 key, no value)
        assert!(eval_str("(assoc {:a 1} :b)", &mut env, &mut d).is_err());
    }

    // --- conj ---

    #[test]
    fn builtin_conj_vector_appends() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(conj [1 2] 3)", &mut env, &mut d),
            Ok(Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_conj_list_prepends() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(conj (list 2 3) 1)", &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_conj_map() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(conj {:a 1} [:b 2])", &mut env, &mut d),
            Ok(Val::Map(vec![
                (Val::Keyword("a".into()), Val::Int(1)),
                (Val::Keyword("b".into()), Val::Int(2)),
            ]))
        );
    }

    #[test]
    fn builtin_conj_too_few_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(conj [1])", &mut env, &mut d).is_err());
    }

    // --- Arithmetic ---

    #[test]
    fn builtin_add_ints() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(+ 1 2 3)", &mut env, &mut d), Ok(Val::Int(6)));
    }

    #[test]
    fn builtin_add_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(+)", &mut env, &mut d), Ok(Val::Int(0)));
    }

    #[test]
    fn builtin_add_float_promotion() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(+ 1 2.0)", &mut env, &mut d), Ok(Val::Float(3.0)));
    }

    #[test]
    fn builtin_add_non_number() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str(r#"(+ 1 "a")"#, &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_sub_two() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(- 10 3)", &mut env, &mut d), Ok(Val::Int(7)));
    }

    #[test]
    fn builtin_sub_negate() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(- 5)", &mut env, &mut d), Ok(Val::Int(-5)));
    }

    #[test]
    fn builtin_sub_empty_error() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(-)", &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_mul_ints() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(* 2 3 4)", &mut env, &mut d), Ok(Val::Int(24)));
    }

    #[test]
    fn builtin_mul_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(*)", &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn builtin_div_ints() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(/ 10 3)", &mut env, &mut d), Ok(Val::Int(3)));
    }

    #[test]
    fn builtin_div_by_zero() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(/ 10 0)", &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_div_wrong_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(/ 1)", &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_mod_ints() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(mod 10 3)", &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn builtin_mod_by_zero() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(mod 10 0)", &mut env, &mut d).is_err());
    }

    // --- Comparison ---

    #[test]
    fn builtin_eq_true() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(= 1 1)", &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_eq_false() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(= 1 2)", &mut env, &mut d), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_eq_wrong_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(= 1)", &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_lt_true() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(< 1 2)", &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_lt_false() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(< 2 1)", &mut env, &mut d), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_gt_true() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(> 2 1)", &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_le_equal() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(<= 2 2)", &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_ge_equal() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(>= 2 2)", &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_comparison_mixed_numeric() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(eval_str("(< 1 2.5)", &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_comparison_non_number() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str(r#"(< 1 "a")"#, &mut env, &mut d).is_err());
    }

    // --- gensym ---

    #[test]
    fn builtin_gensym() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let r1 = eval_str("(gensym)", &mut env, &mut d).unwrap();
        let r2 = eval_str("(gensym)", &mut env, &mut d).unwrap();
        // Each gensym returns a unique symbol
        match (&r1, &r2) {
            (Val::Sym(s1), Val::Sym(s2)) => {
                assert!(s1.starts_with("G__"));
                assert!(s2.starts_with("G__"));
                assert_ne!(s1, s2);
            }
            _ => panic!("gensym should return Sym, got {r1} and {r2}"),
        }
    }

    #[test]
    fn builtin_gensym_no_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(gensym 1)", &mut env, &mut d).is_err());
    }

    // --- apply ---

    #[test]
    fn builtin_apply_builtin_fn() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(apply + (list 1 2 3))", &mut env, &mut d),
            Ok(Val::Int(6))
        );
    }

    #[test]
    fn builtin_apply_user_fn() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        eval_str("(def f (fn [x y] (+ x y)))", &mut env, &mut d).unwrap();
        assert_eq!(
            eval_str("(apply f (list 3 4))", &mut env, &mut d),
            Ok(Val::Int(7))
        );
    }

    #[test]
    fn builtin_apply_with_middle_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (apply + 1 2 (list 3)) → (+ 1 2 3) → 6
        assert_eq!(
            eval_str("(apply + 1 2 (list 3))", &mut env, &mut d),
            Ok(Val::Int(6))
        );
    }

    #[test]
    fn builtin_apply_too_few_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(apply +)", &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_apply_non_collection_last() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(apply + 1 2)", &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_apply_fn_value() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // apply with a fn value (not symbol)
        eval_str("(def f (fn [x] (+ x 1)))", &mut env, &mut d).unwrap();
        assert_eq!(
            eval_str("(apply f [10])", &mut env, &mut d),
            Ok(Val::Int(11))
        );
    }

    // --- Integration: builtins with special forms ---

    #[test]
    fn builtin_in_let() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(let [x (+ 1 2)] (* x 10))", &mut env, &mut d),
            Ok(Val::Int(30))
        );
    }

    #[test]
    fn builtin_in_fn() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        eval_str("(def add (fn [a b] (+ a b)))", &mut env, &mut d).unwrap();
        assert_eq!(eval_str("(add 3 4)", &mut env, &mut d), Ok(Val::Int(7)));
    }

    #[test]
    fn builtin_nested() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(+ (* 2 3) (- 10 4))", &mut env, &mut d),
            Ok(Val::Int(12))
        );
    }

    // =========================================================================
    // defmacro tests
    // =========================================================================

    #[test]
    fn defmacro_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Define a macro that returns a constant form
        eval_str("(defmacro m [] 42)", &mut env, &mut d).unwrap();
        assert_eq!(eval_str("(m)", &mut env, &mut d), Ok(Val::Int(42)));
    }

    #[test]
    fn defmacro_receives_unevaluated_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Macro that receives a form and quotes it (returns it without eval)
        // (defmacro identity-form [x] x) — returns the raw form
        eval_str("(defmacro identity-form [x] x)", &mut env, &mut d).unwrap();
        // (identity-form 42) → eval(42) → 42
        assert_eq!(
            eval_str("(identity-form 42)", &mut env, &mut d),
            Ok(Val::Int(42))
        );
    }

    #[test]
    fn defmacro_expansion_is_re_evaluated() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Macro that constructs a (+ 1 2) form using list and quote
        eval_str(
            r#"(defmacro add12 [] (list (quote +) 1 2))"#,
            &mut env,
            &mut d,
        )
        .unwrap();
        // (add12) → expands to (+ 1 2) → evaluates to 3
        assert_eq!(eval_str("(add12)", &mut env, &mut d), Ok(Val::Int(3)));
    }

    #[test]
    fn defmacro_stored_in_root() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Define macro inside a let — should still be in root
        eval_str("(let [x 1] (defmacro m [] 99))", &mut env, &mut d).unwrap();
        assert_eq!(eval_str("(m)", &mut env, &mut d), Ok(Val::Int(99)));
    }

    #[test]
    fn defmacro_no_name_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(defmacro)", &mut env, &mut d).is_err());
    }

    #[test]
    fn defmacro_no_params_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(defmacro m)", &mut env, &mut d).is_err());
    }

    #[test]
    fn defmacro_non_symbol_name_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(defmacro 42 [] nil)", &mut env, &mut d).is_err());
    }

    #[test]
    fn defmacro_variadic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Macro with variadic args — wraps everything in a list call
        eval_str(
            "(defmacro wrap [& forms] (cons (quote list) forms))",
            &mut env,
            &mut d,
        )
        .unwrap();
        // (wrap 1 2 3) → expands to (list 1 2 3) → (1 2 3)
        assert_eq!(
            eval_str("(wrap 1 2 3)", &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    // --- Integration: defmacro + builtins ---

    #[test]
    fn defmacro_uses_builtins_to_construct_forms() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // A "when" macro: (when test body...) → (if test (do body...) nil)
        eval_str(
            r#"(defmacro when [test & body]
                (list (quote if) test (cons (quote do) body) nil))"#,
            &mut env,
            &mut d,
        )
        .unwrap();
        assert_eq!(
            eval_str("(when true (+ 1 2))", &mut env, &mut d),
            Ok(Val::Int(3))
        );
        assert_eq!(
            eval_str("(when false (+ 1 2))", &mut env, &mut d),
            Ok(Val::Nil)
        );
    }

    #[test]
    fn defmacro_unless_integration() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (unless test body...) → (if test nil (do body...))
        eval_str(
            r#"(defmacro unless [test & body]
                (list (quote if) test nil (cons (quote do) body)))"#,
            &mut env,
            &mut d,
        )
        .unwrap();
        assert_eq!(
            eval_str("(unless false 42)", &mut env, &mut d),
            Ok(Val::Int(42))
        );
        assert_eq!(eval_str("(unless true 42)", &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn defmacro_with_gensym() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Macro that uses gensym to avoid name collisions
        // This just tests that gensym can be called from a macro body
        eval_str(
            "(defmacro test-gensym [] (do (gensym) 42))",
            &mut env,
            &mut d,
        )
        .unwrap();
        assert_eq!(
            eval_str("(test-gensym)", &mut env, &mut d),
            Ok(Val::Int(42))
        );
    }

    // --- concat builtin tests ---

    #[test]
    fn builtin_concat_two_lists() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(concat (list 1 2) (list 3 4))", &mut env, &mut d),
            Ok(Val::List(vec![
                Val::Int(1),
                Val::Int(2),
                Val::Int(3),
                Val::Int(4),
            ]))
        );
    }

    #[test]
    fn builtin_concat_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(concat)", &mut env, &mut d),
            Ok(Val::List(vec![]))
        );
    }

    #[test]
    fn builtin_concat_with_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(concat (list 1) nil (list 2))", &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2)]))
        );
    }

    #[test]
    fn builtin_concat_with_vector() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(concat [1 2] (list 3))", &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_concat_non_seq_error() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert!(eval_str("(concat 42)", &mut env, &mut d).is_err());
    }

    // --- Syntax-quote integration tests ---

    #[test]
    fn syntax_quote_when_macro() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        eval_str(
            "(defmacro when [test & body] `(if ~test (do ~@body) nil))",
            &mut env,
            &mut d,
        )
        .unwrap();
        assert_eq!(
            eval_str("(when true 1 2 3)", &mut env, &mut d),
            Ok(Val::Int(3))
        );
        assert_eq!(
            eval_str("(when false 1 2 3)", &mut env, &mut d),
            Ok(Val::Nil)
        );
    }

    #[test]
    fn syntax_quote_simple_expansion() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Syntax-quote in a let produces a data structure
        assert_eq!(
            eval_str("(let [x 42] `(+ ~x 1))", &mut env, &mut d),
            Ok(Val::List(vec![
                Val::Sym("+".into()),
                Val::Int(42),
                Val::Int(1),
            ]))
        );
    }

    #[test]
    fn syntax_quote_splice_expansion() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        assert_eq!(
            eval_str("(let [xs (list 1 2 3)] `(+ ~@xs))", &mut env, &mut d,),
            Ok(Val::List(vec![
                Val::Sym("+".into()),
                Val::Int(1),
                Val::Int(2),
                Val::Int(3),
            ]))
        );
    }

    #[test]
    fn syntax_quote_unless_macro() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        eval_str(
            "(defmacro unless [test & body] `(if ~test nil (do ~@body)))",
            &mut env,
            &mut d,
        )
        .unwrap();
        assert_eq!(
            eval_str("(unless false 1 2 3)", &mut env, &mut d),
            Ok(Val::Int(3))
        );
        assert_eq!(
            eval_str("(unless true 1 2 3)", &mut env, &mut d),
            Ok(Val::Nil)
        );
    }

    #[test]
    fn syntax_quote_preserves_keywords() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Keywords are self-evaluating — should pass through syntax-quote
        assert_eq!(
            eval_str("`(:a ~(+ 1 2))", &mut env, &mut d),
            Ok(Val::List(vec![Val::Keyword("a".into()), Val::Int(3)]))
        );
    }

    #[test]
    fn unquote_outside_syntax_quote_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let result = eval_str("(unquote x)", &mut env, &mut d);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not inside syntax-quote"));
    }

    #[test]
    fn splice_unquote_outside_syntax_quote_errors() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let result = eval_str("(splice-unquote x)", &mut env, &mut d);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not inside syntax-quote"));
    }
}
