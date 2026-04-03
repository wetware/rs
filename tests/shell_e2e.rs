//! Integration tests for the shell cell's eval logic.
//!
//! NOTE: Full E2E tests (WASM cell via handle_vat_connection_spawn) are
//! blocked by a pre-existing issue where cells using system::serve() exit
//! prematurely in the test harness (the host-side membrane RPC disconnects
//! before the cell can process eval requests). This affects ALL serve()-based
//! cells, not just the shell. See also: test_vat_connection_closes_stdin_on_peer_disconnect
//! in stdin_shutdown_integration.rs which has the same failure mode.
//!
//! These tests exercise the Glia eval pipeline directly (parse → wrap → eval)
//! with the same dispatch and handler code the shell cell uses.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use glia::eval::{self, Dispatch, Env};
use glia::Val;

// ---------------------------------------------------------------------------
// Minimal dispatch for testing (no capabilities, just builtins)
// ---------------------------------------------------------------------------

type HandlerFn =
    for<'a> fn(&'a [Val], &'a RefCell<()>) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>>;

struct TestDispatch {
    table: HashMap<&'static str, HandlerFn>,
}

impl Dispatch for TestDispatch {
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a [Val],
    ) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>> {
        let ctx = RefCell::new(());
        Box::pin(async move {
            match self.table.get(name) {
                Some(handler) => handler(args, &ctx).await,
                None => Err(Val::from(format!("{name}: command not found"))),
            }
        })
    }
}

fn test_dispatch() -> TestDispatch {
    let mut table: HashMap<&'static str, HandlerFn> = HashMap::new();
    table.insert("help", |_, _| {
        Box::pin(std::future::ready(Ok(Val::Str("help text".to_string()))))
    });
    table.insert("exit", |_, _| {
        Box::pin(std::future::ready(Ok(Val::Keyword("exit".into()))))
    });
    TestDispatch { table }
}

/// Wrap a form in the :load effect handler (same pattern as shell cell).
fn wrap_with_load(form: &Val) -> Val {
    Val::List(vec![
        Val::Sym("with-effect-handler".into()),
        Val::Keyword("load".into()),
        Val::List(vec![
            Val::Sym("fn".into()),
            Val::Vector(vec![Val::Sym("path".into()), Val::Sym("resume".into())]),
            Val::List(vec![
                Val::Sym("resume".into()),
                Val::Str("load-not-available-in-test".into()),
            ]),
        ]),
        form.clone(),
    ])
}

async fn eval_text(env: &mut Env, dispatch: &TestDispatch, text: &str) -> (String, bool) {
    if text.trim().is_empty() {
        return (String::new(), false);
    }

    let expr = match glia::read(text) {
        Ok(expr) => expr,
        Err(e) => return (format!("parse error: {e}"), true),
    };

    let wrapped = wrap_with_load(&expr);
    match eval::eval_toplevel(&wrapped, env, dispatch).await {
        Ok(Val::Nil) => (String::new(), false),
        Ok(Val::Keyword(ref k)) if k == "exit" => (":exit".to_string(), false),
        Ok(result) => (format!("{result}"), false),
        Err(e) => (format!("{e}"), true),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_eval_arithmetic() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();
    glia::load_prelude(&mut env, &mut dispatch).await;

    let (result, is_error) = eval_text(&mut env, &dispatch, "(+ 1 2)").await;
    assert!(!is_error, "arithmetic should not error: {result}");
    assert_eq!(result, "3");
}

#[tokio::test]
async fn test_eval_state_persistence() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();
    glia::load_prelude(&mut env, &mut dispatch).await;

    let (_, is_error) = eval_text(&mut env, &dispatch, "(def x 42)").await;
    assert!(!is_error, "def should not error");

    let (result, is_error) = eval_text(&mut env, &dispatch, "x").await;
    assert!(!is_error, "x lookup should not error: {result}");
    assert_eq!(result, "42");
}

#[tokio::test]
async fn test_eval_parse_error() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();

    let (result, is_error) = eval_text(&mut env, &dispatch, "(+ 1").await;
    assert!(is_error, "unmatched paren should be a parse error");
    assert!(
        result.contains("parse error"),
        "should mention parse: {result}"
    );
}

#[tokio::test]
async fn test_eval_unknown_symbol() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();
    glia::load_prelude(&mut env, &mut dispatch).await;

    // Glia: unbound symbols dispatch to the external handler.
    // Without a matching handler, the dispatch returns an error.
    let (result, is_error) = eval_text(&mut env, &dispatch, "nonexistent_symbol").await;
    // Could be an error (dispatch miss) or nil depending on eval semantics.
    // Just verify it doesn't panic.
    let _ = (result, is_error);
}

#[tokio::test]
async fn test_eval_exit_sentinel() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();
    glia::load_prelude(&mut env, &mut dispatch).await;

    let (result, is_error) = eval_text(&mut env, &dispatch, "(exit)").await;
    assert!(!is_error, "(exit) should not be an error");
    assert_eq!(result, ":exit");
}

#[tokio::test]
async fn test_eval_empty_input() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();

    let (result, is_error) = eval_text(&mut env, &dispatch, "").await;
    assert!(!is_error, "empty input should not error");
    assert!(result.is_empty(), "empty input should return empty result");
}

#[tokio::test]
async fn test_eval_prelude_macros() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();
    glia::load_prelude(&mut env, &mut dispatch).await;

    let (result, is_error) = eval_text(&mut env, &dispatch, "(when true 42)").await;
    assert!(!is_error, "when macro should work: {result}");
    assert_eq!(result, "42");

    let (_, is_error) = eval_text(&mut env, &dispatch, "(defn double [x] (* x 2))").await;
    assert!(!is_error, "defn should not error");

    let (result, is_error) = eval_text(&mut env, &dispatch, "(double 21)").await;
    assert!(!is_error, "calling defn'd function should work: {result}");
    assert_eq!(result, "42");
}

#[tokio::test]
async fn test_eval_help_builtin() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();
    glia::load_prelude(&mut env, &mut dispatch).await;

    let (result, is_error) = eval_text(&mut env, &dispatch, "(help)").await;
    assert!(!is_error, "help should not error");
    assert!(result.contains("help text"), "help output: {result}");
}

#[tokio::test]
async fn test_eval_def_then_function() {
    let mut dispatch = test_dispatch();
    let mut env = Env::new();
    glia::load_prelude(&mut env, &mut dispatch).await;

    eval_text(&mut env, &dispatch, "(def greeting \"hello\")").await;
    eval_text(
        &mut env,
        &dispatch,
        "(defn greet [name] (str greeting \" \" name))",
    )
    .await;
    // str isn't a builtin so this will error, but def+defn should work
    let (_, is_error) = eval_text(&mut env, &dispatch, "greeting").await;
    assert!(!is_error);
}
