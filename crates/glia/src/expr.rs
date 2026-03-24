//! Expr — analyzed expression tree for Glia.
//!
//! The pipeline is: `String → Token → Val (parsed) → Expr (analyzed) → Val (result)`.
//! The analyzer (`analyze`) walks a parsed `Val` and produces an `Expr` tree that
//! makes the structure of special forms explicit.  `eval_expr` (in `eval.rs`) then
//! evaluates the `Expr` tree in an environment.
//!
//! Aligns with Clojure's compilation model: fn bodies are analyzed once at
//! definition time (stored as `FnBody::Analyzed`), not re-analyzed on every call.

use crate::Val;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An analyzed expression — the IR between parsed Val and evaluated result.
#[derive(Debug, Clone)]
pub enum Expr {
    /// A constant value (Int, Float, Str, Bool, Nil, Keyword, Bytes, etc.).
    Const(Val),

    /// Symbol lookup — resolved at eval time from Env.
    Sym(String),

    /// `(def name value?)`
    Def { name: String, value: Box<Expr> },

    /// `(if test then else?)`
    If {
        test: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },

    /// `(do exprs...)`
    Do { body: Vec<Expr> },

    /// `(let [bindings...] body...)`
    Let {
        bindings: Vec<(String, Expr)>,
        body: Vec<Expr>,
    },

    /// `(quote val)` — stores the raw Val, returned as-is at eval time.
    Quote(Val),

    /// `(fn [params] body...)` or `(fn (arity1) (arity2) ...)`
    /// Bodies are pre-analyzed.  Captures env at eval time.
    Fn { arities: Vec<FnArityExpr> },

    /// `(loop [bindings...] body...)`
    Loop {
        bindings: Vec<(String, Expr)>,
        body: Vec<Expr>,
    },

    /// `(recur args...)`
    Recur { args: Vec<Expr> },

    /// `(defmacro name [params] body...)` — macro bodies stay as raw Val.
    DefMacro { name: String, raw_args: Vec<Val> },

    /// A function/builtin/dispatch call — unified.
    /// `raw_args` preserved for potential macro expansion (macros need unevaluated Val args).
    Call {
        head: String,
        args: Vec<Expr>,
        raw_args: Vec<Val>,
    },

    /// `(apply fn args...)`
    Apply { args: Vec<Expr>, raw_args: Vec<Val> },

    /// `[exprs...]` — vector literal with analyzed elements.
    Vector(Vec<Expr>),

    /// `{k1 v1 k2 v2 ...}` — map literal with analyzed keys and values.
    Map(Vec<(Expr, Expr)>),

    /// `#{exprs...}` — set literal with analyzed elements.
    Set(Vec<Expr>),
}

/// An analyzed function arity — body forms are pre-analyzed Expr.
#[derive(Debug, Clone)]
pub struct FnArityExpr {
    pub params: Vec<String>,
    pub variadic: Option<String>,
    pub body: Vec<Expr>,
}

/// Fn body storage: raw Val (macro-produced) or pre-analyzed Expr.
///
/// Aligns with Clojure's "compile once at definition time" semantics.
/// The analyzer produces `Analyzed`; macro-generated fns produce `Raw`.
/// `invoke_fn` dispatches on the variant.
#[derive(Debug, Clone)]
pub enum FnBody {
    /// Macro-produced or reader-constructed bodies (not yet analyzed).
    Raw(Vec<Val>),
    /// Pre-analyzed by the analyzer — no re-analysis on invocation.
    Analyzed(Vec<Expr>),
}

// ---------------------------------------------------------------------------
// Analyzer
// ---------------------------------------------------------------------------

/// Analyze a parsed Val into an Expr tree.
///
/// Pure structural transformation — no env or dispatch needed.
/// Macros are NOT expanded here (they depend on runtime env bindings).
pub fn analyze(val: &Val) -> Result<Expr, String> {
    match val {
        // Self-evaluating constants
        Val::Nil
        | Val::Bool(_)
        | Val::Int(_)
        | Val::Float(_)
        | Val::Str(_)
        | Val::Keyword(_)
        | Val::Bytes(_) => Ok(Expr::Const(val.clone())),

        // Symbol — deferred lookup
        Val::Sym(s) => Ok(Expr::Sym(s.clone())),

        // Empty list => nil
        Val::List(items) if items.is_empty() => Ok(Expr::Const(Val::Nil)),

        // Non-empty list => special form or call
        Val::List(items) => analyze_list(items),

        // Collection literals with sub-expression analysis
        Val::Vector(items) => {
            let exprs = items.iter().map(analyze).collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Vector(exprs))
        }
        Val::Map(pairs) => {
            let exprs = pairs
                .iter()
                .map(|(k, v)| -> Result<(Expr, Expr), String> { Ok((analyze(k)?, analyze(v)?)) })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Map(exprs))
        }
        Val::Set(items) => {
            let exprs = items.iter().map(analyze).collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Set(exprs))
        }

        // Runtime-only values (shouldn't appear in source but handle gracefully)
        Val::Fn { .. } | Val::Macro { .. } | Val::Recur(_) => Ok(Expr::Const(val.clone())),
    }
}

/// Analyze a non-empty list form.
fn analyze_list(items: &[Val]) -> Result<Expr, String> {
    let head = match &items[0] {
        Val::Sym(s) => s.as_str(),
        // Non-symbol head: analyze as a call expression
        // e.g. ((fn [x] x) 42) — head is a list, not a symbol
        _ => {
            return Err(format!("expected symbol at head of list, got {}", items[0]));
        }
    };
    let raw_args = &items[1..];

    match head {
        "def" => analyze_def(raw_args),
        "if" => analyze_if(raw_args),
        "do" => analyze_do(raw_args),
        "let" => analyze_let(raw_args),
        "quote" => analyze_quote(raw_args),
        "fn" => analyze_fn(raw_args),
        "loop" => analyze_loop(raw_args),
        "recur" => analyze_recur(raw_args),
        "defmacro" => analyze_defmacro(raw_args),
        "unquote" => Err("unquote (~) not inside syntax-quote".into()),
        "splice-unquote" => Err("splice-unquote (~@) not inside syntax-quote".into()),
        "apply" => {
            let args = raw_args
                .iter()
                .map(analyze)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Apply {
                args,
                raw_args: raw_args.to_vec(),
            })
        }
        _ => {
            let args = raw_args
                .iter()
                .map(analyze)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Call {
                head: head.to_string(),
                args,
                raw_args: raw_args.to_vec(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Individual analyze helpers
// ---------------------------------------------------------------------------

/// `(def name)` or `(def name value)`
fn analyze_def(args: &[Val]) -> Result<Expr, String> {
    if args.is_empty() || args.len() > 2 {
        return Err("def: expected (def name) or (def name value)".into());
    }
    let name = match &args[0] {
        Val::Sym(s) => s.clone(),
        other => return Err(format!("def: expected symbol for name, got {other}")),
    };
    let value = if args.len() == 2 {
        Box::new(analyze(&args[1])?)
    } else {
        Box::new(Expr::Const(Val::Nil))
    };
    Ok(Expr::Def { name, value })
}

/// `(if test then)` or `(if test then else)`
fn analyze_if(args: &[Val]) -> Result<Expr, String> {
    if args.len() < 2 || args.len() > 3 {
        return Err("if: expected 2-3 args (test then else?)".into());
    }
    let test = Box::new(analyze(&args[0])?);
    let then = Box::new(analyze(&args[1])?);
    let else_ = if args.len() == 3 {
        Box::new(analyze(&args[2])?)
    } else {
        Box::new(Expr::Const(Val::Nil))
    };
    Ok(Expr::If { test, then, else_ })
}

/// `(do body...)`
fn analyze_do(args: &[Val]) -> Result<Expr, String> {
    let body = args.iter().map(analyze).collect::<Result<Vec<_>, _>>()?;
    Ok(Expr::Do { body })
}

/// `(let [name1 val1 name2 val2 ...] body...)`
fn analyze_let(args: &[Val]) -> Result<Expr, String> {
    if args.is_empty() {
        return Err("let: expected bindings vector".into());
    }
    let binding_vec = match &args[0] {
        Val::Vector(v) => v,
        other => return Err(format!("let: expected vector for bindings, got {other}")),
    };
    if binding_vec.len() % 2 != 0 {
        return Err("let: bindings must be pairs".into());
    }
    let mut bindings = Vec::new();
    for pair in binding_vec.chunks(2) {
        let name = match &pair[0] {
            Val::Sym(s) => s.clone(),
            other => return Err(format!("let: binding name must be a symbol, got {other}")),
        };
        let expr = analyze(&pair[1])?;
        bindings.push((name, expr));
    }
    let body = args[1..]
        .iter()
        .map(analyze)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Expr::Let { bindings, body })
}

/// `(quote val)`
fn analyze_quote(args: &[Val]) -> Result<Expr, String> {
    if args.len() != 1 {
        return Err(format!("quote: expected 1 arg, got {}", args.len()));
    }
    Ok(Expr::Quote(args[0].clone()))
}

/// `(fn [params] body...)` or `(fn (arity1) (arity2) ...)`
fn analyze_fn(args: &[Val]) -> Result<Expr, String> {
    if args.is_empty() {
        return Err("fn: expected params".into());
    }
    let arities = match &args[0] {
        // Single-arity: (fn [x y] body...)
        Val::Vector(params) => {
            let arity = analyze_fn_arity(params, &args[1..])?;
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
                        result.push(analyze_fn_arity(param_vec, &items[1..])?);
                    }
                    other => return Err(format!("fn: expected arity clause (list), got {other}")),
                }
            }
            // Validate no duplicate arities
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
    Ok(Expr::Fn { arities })
}

/// Parse a single fn arity's params and analyze its body.
fn analyze_fn_arity(param_vec: &[Val], body: &[Val]) -> Result<FnArityExpr, String> {
    let mut params = Vec::new();
    let mut variadic = None;
    let mut i = 0;
    while i < param_vec.len() {
        match &param_vec[i] {
            Val::Sym(s) if s == "&" => {
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
    let analyzed_body = body.iter().map(analyze).collect::<Result<Vec<_>, _>>()?;
    Ok(FnArityExpr {
        params,
        variadic,
        body: analyzed_body,
    })
}

/// `(loop [name1 val1 ...] body...)`
fn analyze_loop(args: &[Val]) -> Result<Expr, String> {
    if args.is_empty() {
        return Err("loop: expected bindings vector".into());
    }
    let binding_vec = match &args[0] {
        Val::Vector(v) => v,
        other => return Err(format!("loop: expected vector for bindings, got {other}")),
    };
    if binding_vec.len() % 2 != 0 {
        return Err("loop: bindings must be pairs".into());
    }
    let mut bindings = Vec::new();
    for pair in binding_vec.chunks(2) {
        let name = match &pair[0] {
            Val::Sym(s) => s.clone(),
            other => return Err(format!("loop: binding name must be a symbol, got {other}")),
        };
        let expr = analyze(&pair[1])?;
        bindings.push((name, expr));
    }
    let body = args[1..]
        .iter()
        .map(analyze)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Expr::Loop { bindings, body })
}

/// `(recur args...)`
fn analyze_recur(args: &[Val]) -> Result<Expr, String> {
    let analyzed = args.iter().map(analyze).collect::<Result<Vec<_>, _>>()?;
    Ok(Expr::Recur { args: analyzed })
}

/// `(defmacro name [params] body...)` — store raw fn-args for eval-time processing.
///
/// The name is extracted and stored separately.  `raw_args` contains only
/// the params and body (`&args[1..]`), NOT the name — avoiding double-storage.
fn analyze_defmacro(args: &[Val]) -> Result<Expr, String> {
    if args.is_empty() {
        return Err("defmacro: expected name".into());
    }
    let name = match &args[0] {
        Val::Sym(s) => s.clone(),
        other => return Err(format!("defmacro: expected symbol for name, got {other}")),
    };
    if args.len() < 2 {
        return Err("defmacro: expected params after name".into());
    }
    Ok(Expr::DefMacro {
        name,
        raw_args: args[1..].to_vec(), // params + body only, no name
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read;

    fn analyze_str(input: &str) -> Result<Expr, String> {
        let val = read(input).map_err(|e| format!("parse: {e}"))?;
        analyze(&val)
    }

    #[test]
    fn analyze_int() {
        match analyze_str("42").unwrap() {
            Expr::Const(Val::Int(42)) => {}
            other => panic!("expected Const(Int(42)), got {other:?}"),
        }
    }

    #[test]
    fn analyze_symbol() {
        match analyze_str("foo").unwrap() {
            Expr::Sym(s) if s == "foo" => {}
            other => panic!("expected Sym(foo), got {other:?}"),
        }
    }

    #[test]
    fn analyze_empty_list() {
        match analyze_str("()").unwrap() {
            Expr::Const(Val::Nil) => {}
            other => panic!("expected Const(Nil), got {other:?}"),
        }
    }

    #[test]
    fn analyze_def_with_value() {
        match analyze_str("(def x 42)").unwrap() {
            Expr::Def { name, value } => {
                assert_eq!(name, "x");
                assert!(matches!(*value, Expr::Const(Val::Int(42))));
            }
            other => panic!("expected Def, got {other:?}"),
        }
    }

    #[test]
    fn analyze_def_no_value() {
        match analyze_str("(def x)").unwrap() {
            Expr::Def { name, value } => {
                assert_eq!(name, "x");
                assert!(matches!(*value, Expr::Const(Val::Nil)));
            }
            other => panic!("expected Def, got {other:?}"),
        }
    }

    #[test]
    fn analyze_if_with_else() {
        match analyze_str("(if true 1 2)").unwrap() {
            Expr::If { test, then, else_ } => {
                assert!(matches!(*test, Expr::Const(Val::Bool(true))));
                assert!(matches!(*then, Expr::Const(Val::Int(1))));
                assert!(matches!(*else_, Expr::Const(Val::Int(2))));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn analyze_if_no_else() {
        match analyze_str("(if true 1)").unwrap() {
            Expr::If { else_, .. } => {
                assert!(matches!(*else_, Expr::Const(Val::Nil)));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn analyze_do() {
        match analyze_str("(do 1 2 3)").unwrap() {
            Expr::Do { body } => assert_eq!(body.len(), 3),
            other => panic!("expected Do, got {other:?}"),
        }
    }

    #[test]
    fn analyze_let() {
        match analyze_str("(let [x 1 y 2] (+ x y))").unwrap() {
            Expr::Let { bindings, body } => {
                assert_eq!(bindings.len(), 2);
                assert_eq!(bindings[0].0, "x");
                assert_eq!(bindings[1].0, "y");
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn analyze_quote() {
        match analyze_str("(quote (a b c))").unwrap() {
            Expr::Quote(Val::List(items)) => assert_eq!(items.len(), 3),
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn analyze_fn_single_arity() {
        match analyze_str("(fn [x y] (+ x y))").unwrap() {
            Expr::Fn { arities } => {
                assert_eq!(arities.len(), 1);
                assert_eq!(arities[0].params, vec!["x", "y"]);
                assert!(arities[0].variadic.is_none());
                assert_eq!(arities[0].body.len(), 1);
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    #[test]
    fn analyze_fn_variadic() {
        match analyze_str("(fn [x & rest] x)").unwrap() {
            Expr::Fn { arities } => {
                assert_eq!(arities[0].params, vec!["x"]);
                assert_eq!(arities[0].variadic, Some("rest".into()));
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    #[test]
    fn analyze_loop() {
        match analyze_str("(loop [i 0] (recur (+ i 1)))").unwrap() {
            Expr::Loop { bindings, body } => {
                assert_eq!(bindings.len(), 1);
                assert_eq!(bindings[0].0, "i");
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected Loop, got {other:?}"),
        }
    }

    #[test]
    fn analyze_recur() {
        match analyze_str("(recur 1 2)").unwrap() {
            Expr::Recur { args } => assert_eq!(args.len(), 2),
            other => panic!("expected Recur, got {other:?}"),
        }
    }

    #[test]
    fn analyze_call() {
        match analyze_str("(+ 1 2)").unwrap() {
            Expr::Call {
                head,
                args,
                raw_args,
            } => {
                assert_eq!(head, "+");
                assert_eq!(args.len(), 2);
                assert_eq!(raw_args.len(), 2);
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn analyze_defmacro() {
        match analyze_str("(defmacro m [x] x)").unwrap() {
            Expr::DefMacro { name, raw_args } => {
                assert_eq!(name, "m");
                assert_eq!(raw_args.len(), 2); // [x] and x (name stored separately)
            }
            other => panic!("expected DefMacro, got {other:?}"),
        }
    }

    #[test]
    fn analyze_vector_literal() {
        match analyze_str("[1 (+ 2 3)]").unwrap() {
            Expr::Vector(exprs) => {
                assert_eq!(exprs.len(), 2);
                assert!(matches!(exprs[0], Expr::Const(Val::Int(1))));
                assert!(matches!(exprs[1], Expr::Call { .. }));
            }
            other => panic!("expected Vector, got {other:?}"),
        }
    }

    #[test]
    fn analyze_apply() {
        match analyze_str("(apply + (list 1 2))").unwrap() {
            Expr::Apply { args, raw_args } => {
                assert_eq!(args.len(), 2);
                assert_eq!(raw_args.len(), 2);
            }
            other => panic!("expected Apply, got {other:?}"),
        }
    }

    #[test]
    fn analyze_unquote_errors() {
        assert!(analyze_str("(unquote x)").is_err());
        assert!(analyze_str("(splice-unquote x)").is_err());
    }

    #[test]
    fn analyze_nested() {
        // (if (> x 0) (do (def y x) y) nil)
        match analyze_str("(if (> x 0) (do (def y x) y) nil)").unwrap() {
            Expr::If { test, then, else_ } => {
                assert!(matches!(*test, Expr::Call { .. }));
                assert!(matches!(*then, Expr::Do { .. }));
                assert!(matches!(*else_, Expr::Const(Val::Nil)));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }
}
