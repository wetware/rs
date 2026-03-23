//! Evaluator for Glia expressions.
//!
//! Resolution order for list forms:
//! 1. Special forms (`def`, `if`, `do`, `let`, `quote`) — unevaluated args
//! 2. Built-in functions (collections, arithmetic, comparison) — evaluated args
//! 3. (future: macro expansion, fn invocation — #207, #209)
//! 4. Generic dispatch — eval args, delegate to [`Dispatch`]
//!
//! Non-list values are self-evaluating (returned as-is), except symbols
//! which are looked up in [`Env`] (unbound symbols pass through).
//!
//! Capability dispatch (host, executor, ipfs, etc.) is provided by the
//! caller via the [`Dispatch`] trait — the evaluator itself is host-agnostic.

use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::Val;

/// Global counter for `gensym`.
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

// ---------------------------------------------------------------------------
// Built-in functions — take already-evaluated args
// ---------------------------------------------------------------------------

/// Try to evaluate `name` as a built-in function with `args`.
/// Returns `None` if `name` is not a builtin (fall through to Dispatch).
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
        "<" => Some(builtin_cmp(args, "<")),
        ">" => Some(builtin_cmp(args, ">")),
        "<=" => Some(builtin_cmp(args, "<=")),
        ">=" => Some(builtin_cmp(args, ">=")),

        // --- Other ---
        "gensym" => Some(builtin_gensym(args)),

        _ => None,
    }
}

// --- Collection builtins ---

fn builtin_cons(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("cons: expected 2 args, got {}", args.len()));
    }
    let tail = match &args[1] {
        Val::List(v) | Val::Vector(v) => v.clone(),
        other => {
            return Err(format!(
                "cons: second arg must be list or vector, got {other}"
            ))
        }
    };
    let mut result = Vec::with_capacity(1 + tail.len());
    result.push(args[0].clone());
    result.extend(tail);
    Ok(Val::List(result))
}

fn builtin_first(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("first: expected 1 arg, got {}", args.len()));
    }
    match &args[0] {
        Val::List(v) | Val::Vector(v) => Ok(v.first().cloned().unwrap_or(Val::Nil)),
        Val::Nil => Ok(Val::Nil),
        other => Err(format!("first: expected list or vector, got {other}")),
    }
}

fn builtin_rest(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("rest: expected 1 arg, got {}", args.len()));
    }
    match &args[0] {
        Val::List(v) | Val::Vector(v) => {
            if v.is_empty() {
                Ok(Val::List(vec![]))
            } else {
                Ok(Val::List(v[1..].to_vec()))
            }
        }
        Val::Nil => Ok(Val::List(vec![])),
        other => Err(format!("rest: expected list or vector, got {other}")),
    }
}

fn builtin_count(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("count: expected 1 arg, got {}", args.len()));
    }
    let n = match &args[0] {
        Val::List(v) | Val::Vector(v) | Val::Set(v) => v.len() as i64,
        Val::Map(pairs) => pairs.len() as i64,
        Val::Str(s) => s.len() as i64,
        Val::Nil => 0,
        other => return Err(format!("count: expected collection, got {other}")),
    };
    Ok(Val::Int(n))
}

fn builtin_vec(args: &[Val]) -> Result<Val, String> {
    if args.len() != 1 {
        return Err(format!("vec: expected 1 arg, got {}", args.len()));
    }
    match &args[0] {
        Val::List(v) => Ok(Val::Vector(v.clone())),
        Val::Vector(_) => Ok(args[0].clone()),
        Val::Nil => Ok(Val::Vector(vec![])),
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
            Val::Int(idx) => {
                if *idx < 0 {
                    return Ok(Val::Nil);
                }
                Ok(v.get(*idx as usize).cloned().unwrap_or(Val::Nil))
            }
            other => Err(format!("get: vector index must be integer, got {other}")),
        },
        Val::Nil => Ok(Val::Nil),
        other => Err(format!("get: expected map or vector, got {other}")),
    }
}

fn builtin_assoc(args: &[Val]) -> Result<Val, String> {
    if args.len() < 3 || (args.len() - 1) % 2 != 0 {
        return Err("assoc: expected (assoc map key val ...)".into());
    }
    match &args[0] {
        Val::Map(pairs) => {
            let mut result = pairs.clone();
            for pair in args[1..].chunks(2) {
                let key = &pair[0];
                let val = &pair[1];
                // Replace existing key if found
                let mut found = false;
                for (k, v) in result.iter_mut() {
                    if k == key {
                        *v = val.clone();
                        found = true;
                        break;
                    }
                }
                if !found {
                    result.push((key.clone(), val.clone()));
                }
            }
            Ok(Val::Map(result))
        }
        other => Err(format!("assoc: expected map, got {other}")),
    }
}

fn builtin_conj(args: &[Val]) -> Result<Val, String> {
    if args.len() < 2 {
        return Err(format!(
            "conj: expected at least 2 args, got {}",
            args.len()
        ));
    }
    match &args[0] {
        Val::List(v) => {
            let mut result = v.clone();
            for item in &args[1..] {
                result.push(item.clone());
            }
            Ok(Val::List(result))
        }
        Val::Vector(v) => {
            let mut result = v.clone();
            for item in &args[1..] {
                result.push(item.clone());
            }
            Ok(Val::Vector(result))
        }
        Val::Map(pairs) => {
            let mut result = pairs.clone();
            for item in &args[1..] {
                match item {
                    Val::Vector(pair) if pair.len() == 2 => {
                        // Replace existing key if found
                        let key = &pair[0];
                        let val = &pair[1];
                        let mut found = false;
                        for (k, v) in result.iter_mut() {
                            if k == key {
                                *v = val.clone();
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            result.push((key.clone(), val.clone()));
                        }
                    }
                    other => {
                        return Err(format!(
                            "conj: map entries must be [key value] vectors, got {other}"
                        ))
                    }
                }
            }
            Ok(Val::Map(result))
        }
        other => Err(format!("conj: expected collection, got {other}")),
    }
}

// --- Arithmetic builtins ---

/// Extract a numeric value, returning (int_val, is_float, float_val).
fn to_number(v: &Val) -> Result<(i64, bool, f64), String> {
    match v {
        Val::Int(n) => Ok((*n, false, *n as f64)),
        Val::Float(n) => Ok((0, true, *n)),
        other => Err(format!("expected number, got {other}")),
    }
}

fn builtin_add(args: &[Val]) -> Result<Val, String> {
    let mut has_float = false;
    let mut int_acc: i64 = 0;
    let mut float_acc: f64 = 0.0;
    for a in args {
        let (i, is_f, f) = to_number(a).map_err(|e| format!("+: {e}"))?;
        if is_f {
            if !has_float {
                float_acc = int_acc as f64;
                has_float = true;
            }
            float_acc += f;
        } else if has_float {
            float_acc += f;
        } else {
            int_acc += i;
        }
    }
    if has_float {
        Ok(Val::Float(float_acc))
    } else {
        Ok(Val::Int(int_acc))
    }
}

fn builtin_sub(args: &[Val]) -> Result<Val, String> {
    if args.is_empty() {
        return Err("-: expected at least 1 arg".into());
    }
    if args.len() == 1 {
        // Unary negate
        let (i, is_f, f) = to_number(&args[0]).map_err(|e| format!("-: {e}"))?;
        return if is_f {
            Ok(Val::Float(-f))
        } else {
            Ok(Val::Int(-i))
        };
    }
    let (first_i, first_is_f, first_f) = to_number(&args[0]).map_err(|e| format!("-: {e}"))?;
    let mut has_float = first_is_f;
    let mut int_acc = first_i;
    let mut float_acc = first_f;
    for a in &args[1..] {
        let (i, is_f, f) = to_number(a).map_err(|e| format!("-: {e}"))?;
        if is_f && !has_float {
            float_acc = int_acc as f64;
            has_float = true;
        }
        if has_float {
            float_acc -= f;
        } else {
            int_acc -= i;
        }
    }
    if has_float {
        Ok(Val::Float(float_acc))
    } else {
        Ok(Val::Int(int_acc))
    }
}

fn builtin_mul(args: &[Val]) -> Result<Val, String> {
    let mut has_float = false;
    let mut int_acc: i64 = 1;
    let mut float_acc: f64 = 1.0;
    for a in args {
        let (i, is_f, f) = to_number(a).map_err(|e| format!("*: {e}"))?;
        if is_f {
            if !has_float {
                float_acc = int_acc as f64;
                has_float = true;
            }
            float_acc *= f;
        } else if has_float {
            float_acc *= f;
        } else {
            int_acc *= i;
        }
    }
    if has_float {
        Ok(Val::Float(float_acc))
    } else {
        Ok(Val::Int(int_acc))
    }
}

fn builtin_div(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("/: expected 2 args, got {}", args.len()));
    }
    let (a_i, a_is_f, a_f) = to_number(&args[0]).map_err(|e| format!("/: {e}"))?;
    let (b_i, b_is_f, b_f) = to_number(&args[1]).map_err(|e| format!("/: {e}"))?;
    if a_is_f || b_is_f {
        if b_f == 0.0 {
            return Err("/: division by zero".into());
        }
        Ok(Val::Float(a_f / b_f))
    } else {
        if b_i == 0 {
            return Err("/: division by zero".into());
        }
        Ok(Val::Int(a_i / b_i))
    }
}

fn builtin_mod(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("mod: expected 2 args, got {}", args.len()));
    }
    match (&args[0], &args[1]) {
        (Val::Int(a), Val::Int(b)) => {
            if *b == 0 {
                return Err("mod: division by zero".into());
            }
            Ok(Val::Int(a % b))
        }
        _ => Err("mod: expected two integers".into()),
    }
}

// --- Comparison builtins ---

fn builtin_eq(args: &[Val]) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("=: expected 2 args, got {}", args.len()));
    }
    Ok(Val::Bool(args[0] == args[1]))
}

fn builtin_cmp(args: &[Val], op: &str) -> Result<Val, String> {
    if args.len() != 2 {
        return Err(format!("{op}: expected 2 args, got {}", args.len()));
    }
    let (a_i, a_is_f, a_f) = to_number(&args[0]).map_err(|e| format!("{op}: {e}"))?;
    let (b_i, b_is_f, b_f) = to_number(&args[1]).map_err(|e| format!("{op}: {e}"))?;
    let result = if a_is_f || b_is_f {
        match op {
            "<" => a_f < b_f,
            ">" => a_f > b_f,
            "<=" => a_f <= b_f,
            ">=" => a_f >= b_f,
            _ => unreachable!(),
        }
    } else {
        match op {
            "<" => a_i < b_i,
            ">" => a_i > b_i,
            "<=" => a_i <= b_i,
            ">=" => a_i >= b_i,
            _ => unreachable!(),
        }
    };
    Ok(Val::Bool(result))
}

// --- Other builtins ---

fn builtin_gensym(args: &[Val]) -> Result<Val, String> {
    if !args.is_empty() {
        return Err(format!("gensym: expected 0 args, got {}", args.len()));
    }
    let n = GENSYM_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    Ok(Val::Sym(format!("G__{n}")))
}

/// Evaluate a Glia expression.
///
/// Resolution order:
/// 1. Special forms — matched by name, receive unevaluated args
/// 2. Built-in functions — eval args, handle in-process
/// 3. (future: macro check, fn invocation — #207, #209)
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

                    // Reserved for future special forms (#207, #208, #209).
                    // Return clear errors so they don't fall through to Dispatch.
                    "fn" => return Err("fn: not yet implemented (see #207)".into()),
                    "loop" => return Err("loop: not yet implemented (see #208)".into()),
                    "recur" => return Err("recur: not yet implemented (see #208)".into()),
                    "defmacro" => return Err("defmacro: not yet implemented (see #209)".into()),

                    _ => {} // fall through to built-ins / generic dispatch
                }

                // --- Built-in functions (evaluated args) ---
                let args = eval_args(raw_args, env, dispatch).await?;

                // `apply` needs dispatch access, so handle it here rather
                // than inside eval_builtin.
                if head == "apply" {
                    if args.len() < 2 {
                        return Err(format!(
                            "apply: expected at least 2 args (fn coll), got {}",
                            args.len()
                        ));
                    }
                    let fn_name = match &args[0] {
                        Val::Sym(s) => s.clone(),
                        other => {
                            return Err(format!("apply: first arg must be symbol, got {other}"))
                        }
                    };
                    // Spread: all middle args are prepended, last arg must be a seq.
                    let last = &args[args.len() - 1];
                    let tail = match last {
                        Val::List(v) | Val::Vector(v) => v.clone(),
                        other => {
                            return Err(format!(
                                "apply: last arg must be list or vector, got {other}"
                            ))
                        }
                    };
                    let mut call_args: Vec<Val> = args[1..args.len() - 1].to_vec();
                    call_args.extend(tail);

                    // Try builtins first, then dispatch to host.
                    if let Some(result) = eval_builtin(&fn_name, &call_args) {
                        return result;
                    }
                    return dispatch.call(&fn_name, &call_args).await;
                }

                if let Some(result) = eval_builtin(head, &args) {
                    return result;
                }

                // --- Generic path: dispatch to host ---
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
        for form in &["fn", "loop", "recur", "defmacro"] {
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

    // =======================================================================
    // Built-in function tests
    // =======================================================================

    // --- list ---

    #[test]
    fn builtin_list_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (list 1 2 3)
        let expr = Val::List(vec![
            Val::Sym("list".into()),
            Val::Int(1),
            Val::Int(2),
            Val::Int(3),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_list_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (list)
        let expr = Val::List(vec![Val::Sym("list".into())]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::List(vec![]))
        );
    }

    // --- cons ---

    #[test]
    fn builtin_cons_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (cons 1 [2 3])
        let expr = Val::List(vec![
            Val::Sym("cons".into()),
            Val::Int(1),
            Val::Vector(vec![Val::Int(2), Val::Int(3)]),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_cons_wrong_second_arg() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (cons 1 2)
        let expr = Val::List(vec![Val::Sym("cons".into()), Val::Int(1), Val::Int(2)]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- first ---

    #[test]
    fn builtin_first_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (first [1 2 3])
        let expr = Val::List(vec![
            Val::Sym("first".into()),
            Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn builtin_first_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (first [])
        let expr = Val::List(vec![Val::Sym("first".into()), Val::Vector(vec![])]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn builtin_first_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (first nil)
        let expr = Val::List(vec![Val::Sym("first".into()), Val::Nil]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Nil));
    }

    // --- rest ---

    #[test]
    fn builtin_rest_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (rest [1 2 3])
        let expr = Val::List(vec![
            Val::Sym("rest".into()),
            Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::List(vec![Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_rest_empty() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (rest [])
        let expr = Val::List(vec![Val::Sym("rest".into()), Val::Vector(vec![])]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::List(vec![]))
        );
    }

    // --- count ---

    #[test]
    fn builtin_count_vector() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (count [1 2 3])
        let expr = Val::List(vec![
            Val::Sym("count".into()),
            Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(3)));
    }

    #[test]
    fn builtin_count_string() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (count "hello")
        let expr = Val::List(vec![Val::Sym("count".into()), Val::Str("hello".into())]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(5)));
    }

    #[test]
    fn builtin_count_nil() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (count nil)
        let expr = Val::List(vec![Val::Sym("count".into()), Val::Nil]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(0)));
    }

    #[test]
    fn builtin_count_map() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (count {:a 1 :b 2})
        let expr = Val::List(vec![
            Val::Sym("count".into()),
            Val::Map(vec![
                (Val::Keyword("a".into()), Val::Int(1)),
                (Val::Keyword("b".into()), Val::Int(2)),
            ]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(2)));
    }

    // --- vec ---

    #[test]
    fn builtin_vec_from_list() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // Use (list 1 2) so the inner list evaluates to Val::List
        let expr = Val::List(vec![
            Val::Sym("vec".into()),
            Val::List(vec![Val::Sym("list".into()), Val::Int(1), Val::Int(2)]),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Vector(vec![Val::Int(1), Val::Int(2)]))
        );
    }

    #[test]
    fn builtin_vec_error_on_int() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("vec".into()), Val::Int(42)]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- get ---

    #[test]
    fn builtin_get_map() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (get {:a 1 :b 2} :a)
        let expr = Val::List(vec![
            Val::Sym("get".into()),
            Val::Map(vec![
                (Val::Keyword("a".into()), Val::Int(1)),
                (Val::Keyword("b".into()), Val::Int(2)),
            ]),
            Val::Keyword("a".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn builtin_get_map_not_found() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![
            Val::Sym("get".into()),
            Val::Map(vec![(Val::Keyword("a".into()), Val::Int(1))]),
            Val::Keyword("z".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Nil));
    }

    #[test]
    fn builtin_get_vector() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (get [10 20 30] 1)
        let expr = Val::List(vec![
            Val::Sym("get".into()),
            Val::Vector(vec![Val::Int(10), Val::Int(20), Val::Int(30)]),
            Val::Int(1),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(20)));
    }

    #[test]
    fn builtin_get_vector_out_of_bounds() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![
            Val::Sym("get".into()),
            Val::Vector(vec![Val::Int(10)]),
            Val::Int(5),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Nil));
    }

    // --- assoc ---

    #[test]
    fn builtin_assoc_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (assoc {:a 1} :b 2)
        let expr = Val::List(vec![
            Val::Sym("assoc".into()),
            Val::Map(vec![(Val::Keyword("a".into()), Val::Int(1))]),
            Val::Keyword("b".into()),
            Val::Int(2),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d).unwrap();
        match result {
            Val::Map(pairs) => {
                assert_eq!(pairs.len(), 2);
            }
            other => panic!("expected map, got {other}"),
        }
    }

    #[test]
    fn builtin_assoc_replace() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (assoc {:a 1} :a 99)
        let expr = Val::List(vec![
            Val::Sym("assoc".into()),
            Val::Map(vec![(Val::Keyword("a".into()), Val::Int(1))]),
            Val::Keyword("a".into()),
            Val::Int(99),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d).unwrap();
        match result {
            Val::Map(pairs) => {
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].1, Val::Int(99));
            }
            other => panic!("expected map, got {other}"),
        }
    }

    #[test]
    fn builtin_assoc_not_map() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![
            Val::Sym("assoc".into()),
            Val::Int(42),
            Val::Keyword("a".into()),
            Val::Int(1),
        ]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- conj ---

    #[test]
    fn builtin_conj_vector() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (conj [1 2] 3)
        let expr = Val::List(vec![
            Val::Sym("conj".into()),
            Val::Vector(vec![Val::Int(1), Val::Int(2)]),
            Val::Int(3),
        ]);
        assert_eq!(
            eval_blocking(&expr, &mut env, &mut d),
            Ok(Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]))
        );
    }

    #[test]
    fn builtin_conj_map() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (conj {:a 1} [:b 2])
        let expr = Val::List(vec![
            Val::Sym("conj".into()),
            Val::Map(vec![(Val::Keyword("a".into()), Val::Int(1))]),
            Val::Vector(vec![Val::Keyword("b".into()), Val::Int(2)]),
        ]);
        let result = eval_blocking(&expr, &mut env, &mut d).unwrap();
        match result {
            Val::Map(pairs) => assert_eq!(pairs.len(), 2),
            other => panic!("expected map, got {other}"),
        }
    }

    #[test]
    fn builtin_conj_map_bad_entry() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (conj {:a 1} 42) — not a vector pair
        let expr = Val::List(vec![
            Val::Sym("conj".into()),
            Val::Map(vec![(Val::Keyword("a".into()), Val::Int(1))]),
            Val::Int(42),
        ]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- + ---

    #[test]
    fn builtin_add_ints() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (+ 1 2 3)
        let expr = Val::List(vec![
            Val::Sym("+".into()),
            Val::Int(1),
            Val::Int(2),
            Val::Int(3),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(6)));
    }

    #[test]
    fn builtin_add_mixed() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (+ 1 2.5)
        let expr = Val::List(vec![Val::Sym("+".into()), Val::Int(1), Val::Float(2.5)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Float(3.5)));
    }

    #[test]
    fn builtin_add_zero_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (+)
        let expr = Val::List(vec![Val::Sym("+".into())]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(0)));
    }

    #[test]
    fn builtin_add_non_number() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![
            Val::Sym("+".into()),
            Val::Int(1),
            Val::Str("x".into()),
        ]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- - ---

    #[test]
    fn builtin_sub_binary() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (- 5 3)
        let expr = Val::List(vec![Val::Sym("-".into()), Val::Int(5), Val::Int(3)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(2)));
    }

    #[test]
    fn builtin_sub_unary() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (- 5)
        let expr = Val::List(vec![Val::Sym("-".into()), Val::Int(5)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(-5)));
    }

    #[test]
    fn builtin_sub_no_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("-".into())]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- * ---

    #[test]
    fn builtin_mul_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (* 2 3)
        let expr = Val::List(vec![Val::Sym("*".into()), Val::Int(2), Val::Int(3)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(6)));
    }

    #[test]
    fn builtin_mul_zero_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (*)
        let expr = Val::List(vec![Val::Sym("*".into())]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    // --- / ---

    #[test]
    fn builtin_div_int() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (/ 10 3)
        let expr = Val::List(vec![Val::Sym("/".into()), Val::Int(10), Val::Int(3)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(3)));
    }

    #[test]
    fn builtin_div_float() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (/ 10.0 3)
        let expr = Val::List(vec![Val::Sym("/".into()), Val::Float(10.0), Val::Int(3)]);
        let result = eval_blocking(&expr, &mut env, &mut d).unwrap();
        match result {
            Val::Float(f) => assert!((f - 10.0 / 3.0).abs() < 1e-10),
            other => panic!("expected float, got {other}"),
        }
    }

    #[test]
    fn builtin_div_by_zero() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("/".into()), Val::Int(10), Val::Int(0)]);
        let result = eval_blocking(&expr, &mut env, &mut d);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("division by zero"));
    }

    // --- mod ---

    #[test]
    fn builtin_mod_basic() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (mod 10 3)
        let expr = Val::List(vec![Val::Sym("mod".into()), Val::Int(10), Val::Int(3)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(1)));
    }

    #[test]
    fn builtin_mod_by_zero() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("mod".into()), Val::Int(10), Val::Int(0)]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- = ---

    #[test]
    fn builtin_eq_true() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (= 1 1)
        let expr = Val::List(vec![Val::Sym("=".into()), Val::Int(1), Val::Int(1)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_eq_false() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (= 1 2)
        let expr = Val::List(vec![Val::Sym("=".into()), Val::Int(1), Val::Int(2)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_eq_different_types() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (= 1 "1")
        let expr = Val::List(vec![
            Val::Sym("=".into()),
            Val::Int(1),
            Val::Str("1".into()),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Bool(false)));
    }

    // --- < > <= >= ---

    #[test]
    fn builtin_lt() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("<".into()), Val::Int(1), Val::Int(2)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_gt() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym(">".into()), Val::Int(2), Val::Int(1)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_lte() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("<=".into()), Val::Int(2), Val::Int(2)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Bool(true)));
    }

    #[test]
    fn builtin_gte_false() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym(">=".into()), Val::Int(1), Val::Int(2)]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Bool(false)));
    }

    #[test]
    fn builtin_cmp_non_numeric() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![
            Val::Sym("<".into()),
            Val::Str("a".into()),
            Val::Str("b".into()),
        ]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- gensym ---

    #[test]
    fn builtin_gensym_unique() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("gensym".into())]);
        let a = eval_blocking(&expr, &mut env, &mut d).unwrap();
        let b = eval_blocking(&expr, &mut env, &mut d).unwrap();
        assert_ne!(a, b);
        match a {
            Val::Sym(s) => assert!(s.starts_with("G__")),
            other => panic!("expected sym, got {other}"),
        }
    }

    #[test]
    fn builtin_gensym_wrong_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        let expr = Val::List(vec![Val::Sym("gensym".into()), Val::Int(1)]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- apply ---

    #[test]
    fn builtin_apply_to_builtin() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (apply + [1 2 3])
        let expr = Val::List(vec![
            Val::Sym("apply".into()),
            Val::Sym("+".into()),
            Val::Vector(vec![Val::Int(1), Val::Int(2), Val::Int(3)]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(6)));
    }

    #[test]
    fn builtin_apply_with_leading_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (apply + 0 [1 2])  =  (+ 0 1 2)
        let expr = Val::List(vec![
            Val::Sym("apply".into()),
            Val::Sym("+".into()),
            Val::Int(0),
            Val::Vector(vec![Val::Int(1), Val::Int(2)]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(3)));
    }

    #[test]
    fn builtin_apply_to_dispatch() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (apply host ["id"])
        let expr = Val::List(vec![
            Val::Sym("apply".into()),
            Val::Sym("host".into()),
            Val::Vector(vec![Val::Str("id".into())]),
        ]);
        eval_blocking(&expr, &mut env, &mut d).unwrap();
        assert_eq!(d.calls.len(), 1);
        assert_eq!(d.calls[0].0, "host");
        assert_eq!(d.calls[0].1, vec![Val::Str("id".into())]);
    }

    #[test]
    fn builtin_apply_bad_last_arg() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (apply + 1 2) — last arg not a seq
        let expr = Val::List(vec![
            Val::Sym("apply".into()),
            Val::Sym("+".into()),
            Val::Int(1),
            Val::Int(2),
        ]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    #[test]
    fn builtin_apply_too_few_args() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (apply +) — only 1 arg
        let expr = Val::List(vec![Val::Sym("apply".into()), Val::Sym("+".into())]);
        assert!(eval_blocking(&expr, &mut env, &mut d).is_err());
    }

    // --- builtins do NOT dispatch to host ---

    #[test]
    fn builtins_do_not_dispatch() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (+ 1 2) should NOT go through dispatch
        let expr = Val::List(vec![Val::Sym("+".into()), Val::Int(1), Val::Int(2)]);
        eval_blocking(&expr, &mut env, &mut d).unwrap();
        assert!(d.calls.is_empty(), "builtins should not call dispatch");
    }

    // --- builtins compose with special forms ---

    #[test]
    fn builtin_in_let() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (let [x 10] (+ x 5))
        let expr = Val::List(vec![
            Val::Sym("let".into()),
            Val::Vector(vec![Val::Sym("x".into()), Val::Int(10)]),
            Val::List(vec![
                Val::Sym("+".into()),
                Val::Sym("x".into()),
                Val::Int(5),
            ]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(15)));
    }

    #[test]
    fn nested_builtin_calls() {
        let mut env = Env::new();
        let mut d = RecordingDispatch::new();
        // (+ 1 (* 2 3))
        let expr = Val::List(vec![
            Val::Sym("+".into()),
            Val::Int(1),
            Val::List(vec![Val::Sym("*".into()), Val::Int(2), Val::Int(3)]),
        ]);
        assert_eq!(eval_blocking(&expr, &mut env, &mut d), Ok(Val::Int(7)));
    }
}
