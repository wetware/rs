//! Evaluator for Glia expressions.
//!
//! Resolution order for list forms:
//! 1. Special forms (`def`, `if`, `do`, `let`, `fn`, `quote`) — unevaluated args
//! 2. Env lookup — if head resolves to `Val::Fn`, invoke the closure
//! 3. Built-in functions (`+`, `list`, `cons`, `apply`, etc.)
//! 4. (future: macro expansion — #209)
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

// ---------------------------------------------------------------------------
// Built-in functions
// ---------------------------------------------------------------------------

/// Try to evaluate `name` as a built-in function with already-evaluated `args`.
///
/// Returns `None` when `name` is not a built-in (caller should fall through to
/// Dispatch).  Returns `Some(Ok(val))` or `Some(Err(msg))` for built-ins.
///
/// NOTE: `apply` is handled separately in `eval()` because it needs to
/// re-dispatch through the evaluator.
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

/// Evaluate a Glia expression.
///
/// Resolution order:
/// 1. Special forms — matched by name, receive unevaluated args
/// 2. Env lookup — if head resolves to Val::Fn, invoke it
/// 3. Built-in functions — pure functions like +, list, cons, etc.
/// 4. Generic path — eval args, delegate to Dispatch (capability calls)
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

                    // Reserved for future special forms (#208, #209).
                    "loop" => return Err("loop: not yet implemented (see #208)".into()),
                    "recur" => return Err("recur: not yet implemented (see #208)".into()),
                    "defmacro" => return Err("defmacro: not yet implemented (see #209)".into()),

                    _ => {} // fall through to env lookup / builtins / dispatch
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
        for form in &["loop", "recur", "defmacro"] {
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

    // =====================================================================
    // Built-in function tests
    // =====================================================================

    /// Helper: parse + eval, return result.
    fn eval_str(src: &str) -> Result<Val, String> {
        let expr = crate::read(src)?;
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        eval_blocking(&expr, &mut env, &mut d)
    }

    /// Helper: parse + eval with a shared env (for multi-step tests).
    fn eval_str_env(src: &str, env: &mut Env, d: &mut RecordingDispatch) -> Result<Val, String> {
        let expr = crate::read(src)?;
        eval_blocking(&expr, env, d)
    }

    // --- list ---

    #[test]
    fn builtin_list_creates_list() {
        assert_eq!(
            eval_str("(list 1 2 3)"),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_list_empty() {
        assert_eq!(eval_str("(list)"), Ok(Val::List(vec![])));
    }

    // --- cons ---

    #[test]
    fn builtin_cons_prepends() {
        assert_eq!(
            eval_str("(cons 1 (list 2 3))"),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_cons_non_collection_errors() {
        assert!(eval_str("(cons 1 2)").is_err());
    }

    // --- first ---

    #[test]
    fn builtin_first_gets_head() {
        assert_eq!(eval_str("(first (list 10 20 30))"), Ok(Val::Int(10)));
    }

    #[test]
    fn builtin_first_empty_is_nil() {
        assert_eq!(eval_str("(first (list))"), Ok(Val::Nil));
    }

    #[test]
    fn builtin_first_nil_is_nil() {
        assert_eq!(eval_str("(first nil)"), Ok(Val::Nil));
    }

    // --- rest ---

    #[test]
    fn builtin_rest_returns_tail() {
        assert_eq!(
            eval_str("(rest (list 1 2 3))"),
            Ok(Val::List(vec![Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_rest_empty_is_empty() {
        assert_eq!(eval_str("(rest (list))"), Ok(Val::List(vec![])));
    }

    // --- count ---

    #[test]
    fn builtin_count_list() {
        assert_eq!(eval_str("(count (list 1 2 3))"), Ok(Val::Int(3)));
    }

    #[test]
    fn builtin_count_string_chars() {
        // Multi-byte chars: count should be char count, not byte length.
        assert_eq!(eval_str(r#"(count "héllo")"#), Ok(Val::Int(5)));
    }

    #[test]
    fn builtin_count_nil() {
        assert_eq!(eval_str("(count nil)"), Ok(Val::Int(0)));
    }

    #[test]
    fn builtin_count_wrong_type() {
        assert!(eval_str("(count 42)").is_err());
    }

    // --- vec ---

    #[test]
    fn builtin_vec_converts_list() {
        assert_eq!(
            eval_str("(vec (list 1 2))"),
            Ok(Val::Vector(vec![Val::Int(1), Val::Int(2)]))
        );
    }

    #[test]
    fn builtin_vec_nil_is_empty_vector() {
        assert_eq!(eval_str("(vec nil)"), Ok(Val::Vector(vec![])));
    }

    // --- get ---

    #[test]
    fn builtin_get_map() {
        assert_eq!(eval_str("(get {:a 1 :b 2} :b)"), Ok(Val::Int(2)));
    }

    #[test]
    fn builtin_get_map_missing() {
        assert_eq!(eval_str("(get {:a 1} :z)"), Ok(Val::Nil));
    }

    #[test]
    fn builtin_get_vector_index() {
        assert_eq!(eval_str("(get [10 20 30] 1)"), Ok(Val::Int(20)));
    }

    #[test]
    fn builtin_get_vector_out_of_bounds() {
        assert_eq!(eval_str("(get [10 20] 5)"), Ok(Val::Nil));
    }

    // --- assoc ---

    #[test]
    fn builtin_assoc_adds_key() {
        let result = eval_str("(assoc {:a 1} :b 2)").unwrap();
        match result {
            Val::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert!(pairs.contains(&(Val::Keyword("b".into()), Val::Int(2))));
            }
            other => panic!("expected map, got {other}"),
        }
    }

    #[test]
    fn builtin_assoc_bad_arity() {
        assert!(eval_str("(assoc {:a 1} :b)").is_err());
    }

    // --- conj ---

    #[test]
    fn builtin_conj_vector_appends() {
        assert_eq!(
            eval_str("(conj [1 2] 3)"),
            Ok(Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_conj_list_prepends() {
        // Clojure semantics: conj on list PREPENDS
        assert_eq!(
            eval_str("(conj (list 2 3) 1)"),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_conj_map() {
        let result = eval_str("(conj {:a 1} [:b 2])").unwrap();
        match result {
            Val::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert!(pairs.contains(&(Val::Keyword("b".into()), Val::Int(2))));
            }
            other => panic!("expected map, got {other}"),
        }
    }

    #[test]
    fn builtin_conj_too_few_args() {
        assert!(eval_str("(conj [1])").is_err());
    }

    // --- Arithmetic ---

    #[test]
    fn builtin_add() {
        assert_eq!(eval_str("(+ 1 2 3)"), Ok(Val::Int(6)));
    }

    #[test]
    fn builtin_add_empty() {
        assert_eq!(eval_str("(+)"), Ok(Val::Int(0)));
    }

    #[test]
    fn builtin_add_mixed_promotes() {
        assert_eq!(eval_str("(+ 1 2.0)"), Ok(Val::Float(3.0)));
    }

    #[test]
    fn builtin_sub() {
        assert_eq!(eval_str("(- 10 3)"), Ok(Val::Int(7)));
    }

    #[test]
    fn builtin_sub_unary_negate() {
        assert_eq!(eval_str("(- 5)"), Ok(Val::Int(-5)));
    }

    #[test]
    fn builtin_sub_no_args() {
        assert!(eval_str("(-)").is_err());
    }

    #[test]
    fn builtin_mul() {
        assert_eq!(eval_str("(* 2 3 4)"), Ok(Val::Int(24)));
    }

    #[test]
    fn builtin_mul_empty() {
        assert_eq!(eval_str("(*)"), Ok(Val::Int(1)));
    }

    #[test]
    fn builtin_div_int() {
        assert_eq!(eval_str("(/ 10 3)"), Ok(Val::Int(3)));
    }

    #[test]
    fn builtin_div_float() {
        assert_eq!(eval_str("(/ 10.0 4)"), Ok(Val::Float(2.5)));
    }

    #[test]
    fn builtin_div_by_zero() {
        assert!(eval_str("(/ 1 0)").is_err());
    }

    #[test]
    fn builtin_mod_basic() {
        assert_eq!(eval_str("(mod 10 3)"), Ok(Val::Int(1)));
    }

    #[test]
    fn builtin_mod_by_zero() {
        assert!(eval_str("(mod 10 0)").is_err());
    }

    // --- Comparison ---

    #[test]
    fn builtin_eq_true() {
        assert_eq!(eval_str("(= 1 1)"), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_eq_false() {
        assert_eq!(eval_str("(= 1 2)"), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_lt() {
        assert_eq!(eval_str("(< 1 2)"), Ok(Val::Bool(true)));
        assert_eq!(eval_str("(< 2 1)"), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_gt() {
        assert_eq!(eval_str("(> 3 1)"), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_le() {
        assert_eq!(eval_str("(<= 1 1)"), Ok(Val::Bool(true)));
        assert_eq!(eval_str("(<= 1 2)"), Ok(Val::Bool(true)));
        assert_eq!(eval_str("(<= 2 1)"), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_ge() {
        assert_eq!(eval_str("(>= 2 2)"), Ok(Val::Bool(true)));
        assert_eq!(eval_str("(>= 1 2)"), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_comparison_cross_type() {
        assert_eq!(eval_str("(< 1 2.5)"), Ok(Val::Bool(true)));
        assert_eq!(eval_str("(> 3.0 2)"), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_comparison_non_numeric_errors() {
        assert!(eval_str(r#"(< "a" "b")"#).is_err());
    }

    // --- gensym ---

    #[test]
    fn builtin_gensym_increments() {
        let a = eval_str("(gensym)").unwrap();
        let b = eval_str("(gensym)").unwrap();
        // Both should be symbols starting with G__
        match (&a, &b) {
            (Val::Sym(sa), Val::Sym(sb)) => {
                assert!(sa.starts_with("G__"));
                assert!(sb.starts_with("G__"));
                assert_ne!(sa, sb);
            }
            _ => panic!("expected symbols"),
        }
    }

    #[test]
    fn builtin_gensym_no_args_only() {
        assert!(eval_str("(gensym 1)").is_err());
    }

    // --- apply ---

    #[test]
    fn builtin_apply_builtin_fn() {
        assert_eq!(eval_str("(apply + [1 2 3])"), Ok(Val::Int(6)));
    }

    #[test]
    fn builtin_apply_with_middle_args() {
        // (apply + 0 [1 2]) → (+ 0 1 2) → 3
        assert_eq!(eval_str("(apply + 0 [1 2])"), Ok(Val::Int(3)));
    }

    #[test]
    fn builtin_apply_user_fn() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        eval_str_env("(def add (fn [a b] (+ a b)))", &mut env, &mut d).unwrap();
        let result = eval_str_env("(apply add [3 4])", &mut env, &mut d);
        assert_eq!(result, Ok(Val::Int(7)));
    }

    #[test]
    fn builtin_apply_last_not_collection() {
        assert!(eval_str("(apply + 1 2)").is_err());
    }

    #[test]
    fn builtin_apply_too_few_args() {
        assert!(eval_str("(apply +)").is_err());
    }
}
