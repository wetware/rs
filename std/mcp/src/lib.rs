//! MCP cell — JSON-RPC server for AI agent integration.
//!
//! A raw cell that speaks MCP (Model Context Protocol) over WASI
//! stdin/stdout.  Grafts the membrane to obtain capabilities, sets
//! up a Glia evaluator, and serves JSON-RPC requests.
//!
//! The cell exposes a single MCP tool: `eval`, which evaluates Glia
//! expressions against the node's membrane capabilities.
//!
//! ```text
//! Claude Code -> stdin/stdout -> MCP cell (WASM) -> Glia eval -> membrane caps
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::io::{BufRead, Write};
use std::pin::Pin;
use std::rc::Rc;

use glia::eval::{self, Dispatch, Env};
use glia::Val;

use wasip2::exports::cli::run::Guest;

// Shared effect handler factories from the caps crate.
use caps::{
    eval_load, get_graft_cap, make_host_handler, make_import_handler, make_ipfs_handler,
    make_routing_handler, routing_capnp, stem_capnp, system_capnp, wrap_with_handlers,
};

type Membrane = stem_capnp::membrane::Client;

// ---------------------------------------------------------------------------
// JSON-RPC types (minimal, hand-rolled to avoid pulling in jsonrpc crate)
// ---------------------------------------------------------------------------

/// Incoming JSON-RPC request.
#[derive(serde::Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    method: String,
    params: Option<serde_json::Value>,
    id: Option<serde_json::Value>,
}

/// Write a JSON-RPC success response to stdout.
fn write_result(id: &serde_json::Value, result: serde_json::Value) {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "result": result,
        "id": id,
    });
    let mut out = std::io::stdout();
    let _ = serde_json::to_writer(&mut out, &resp);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Write a JSON-RPC error response to stdout.
fn write_error(id: &serde_json::Value, code: i64, message: &str) {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": code,
            "message": message,
        },
        "id": id,
    });
    let mut out = std::io::stdout();
    let _ = serde_json::to_writer(&mut out, &resp);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

// ---------------------------------------------------------------------------
// MCP protocol constants
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "wetware";
const SERVER_VERSION: &str = "0.1.0";

fn initialize_result() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
        },
        "capabilities": {
            "tools": {},
        },
    })
}

fn tools_list_result() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            {
                "name": "eval",
                "description": "Evaluate a Glia expression on the Wetware node. Returns the result as text. Examples: (perform host :id), (perform host :peers), (help)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "expression": {
                            "type": "string",
                            "description": "Glia expression to evaluate"
                        }
                    },
                    "required": ["expression"]
                }
            }
        ]
    })
}

// ---------------------------------------------------------------------------
// Dispatch — delegates to shared caps crate handlers
// ---------------------------------------------------------------------------

type HandlerFn = for<'a> fn(
    &'a [Val],
    &'a RefCell<McpSession>,
) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>>;

struct McpSession {
    #[allow(dead_code)]
    host: Option<system_capnp::host::Client>,
    #[allow(dead_code)]
    routing: Option<routing_capnp::routing::Client>,
}

struct McpDispatch<'s> {
    ctx: &'s RefCell<McpSession>,
    table: &'s HashMap<&'static str, HandlerFn>,
}

impl<'s> Dispatch for McpDispatch<'s> {
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a [Val],
    ) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>> {
        Box::pin(async move {
            match self.table.get(name) {
                Some(handler) => handler(args, self.ctx).await,
                None => Err(Val::from(format!("{name}: command not found"))),
            }
        })
    }
}

fn build_dispatch() -> HashMap<&'static str, HandlerFn> {
    let mut t: HashMap<&'static str, HandlerFn> = HashMap::new();
    t.insert("load", |a, _| Box::pin(std::future::ready(eval_load(a))));
    t.insert("help", |_, _| {
        Box::pin(std::future::ready(Ok(Val::Str(HELP_TEXT.to_string()))))
    });
    t
}

const HELP_TEXT: &str = "\
MCP Glia evaluator. Available commands:
  (perform host :id)       - peer identity
  (perform host :addrs)    - listen addresses
  (perform host :peers)    - connected peers
  (perform routing :find \"name\") - DHT lookup
  (help)                   - this message";

// ---------------------------------------------------------------------------
// MCP JSON-RPC server loop
// ---------------------------------------------------------------------------

/// Evaluate a Glia expression and return the result as a string.
async fn eval_expression(
    expr_text: &str,
    env: &RefCell<Env>,
    ctx: &RefCell<McpSession>,
    dispatch_table: &HashMap<&'static str, HandlerFn>,
) -> Result<String, String> {
    let expr = glia::read(expr_text).map_err(|e| format!("parse error: {e}"))?;

    let wrapped = wrap_with_handlers(&expr, &[]);
    let dispatch = McpDispatch {
        ctx,
        table: dispatch_table,
    };
    match eval::eval_toplevel(&wrapped, &mut env.borrow_mut(), &dispatch).await {
        Ok(Val::Nil) => Ok("nil".to_string()),
        Ok(result) => Ok(format!("{result}")),
        Err(e) => Err(format!("{e}")),
    }
}

/// Handle a single JSON-RPC request and write the response to stdout.
///
/// Returns `true` to continue the loop, `false` on exit conditions.
async fn handle_request(
    line: &str,
    env: &RefCell<Env>,
    ctx: &RefCell<McpSession>,
    dispatch_table: &HashMap<&'static str, HandlerFn>,
) -> bool {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            // Parse error: respond with JSON-RPC error if we can.
            let null_id = serde_json::Value::Null;
            write_error(&null_id, -32700, &format!("Parse error: {e}"));
            return true;
        }
    };

    // Notifications (no id) get no response.
    let id = match req.id {
        Some(ref id) => id.clone(),
        None => return true, // notification — no response needed
    };

    match req.method.as_str() {
        "initialize" => {
            write_result(&id, initialize_result());
        }
        "ping" => {
            write_result(&id, serde_json::json!({}));
        }
        "tools/list" => {
            write_result(&id, tools_list_result());
        }
        "tools/call" => {
            // Extract tool name and arguments.
            let params = req.params.unwrap_or(serde_json::Value::Null);
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match tool_name {
                "eval" => {
                    let expression = params
                        .get("arguments")
                        .and_then(|a| a.get("expression"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if expression.is_empty() {
                        write_result(
                            &id,
                            serde_json::json!({
                                "content": [{
                                    "type": "text",
                                    "text": "error: empty expression"
                                }],
                                "isError": true,
                            }),
                        );
                    } else {
                        match eval_expression(expression, env, ctx, dispatch_table).await {
                            Ok(result) => {
                                write_result(
                                    &id,
                                    serde_json::json!({
                                        "content": [{
                                            "type": "text",
                                            "text": result,
                                        }],
                                    }),
                                );
                            }
                            Err(err) => {
                                write_result(
                                    &id,
                                    serde_json::json!({
                                        "content": [{
                                            "type": "text",
                                            "text": err,
                                        }],
                                        "isError": true,
                                    }),
                                );
                            }
                        }
                    }
                }
                _ => {
                    write_error(&id, -32602, &format!("Unknown tool: {tool_name}"));
                }
            }
        }
        _ => {
            write_error(&id, -32601, &format!("Method not found: {}", req.method));
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

struct McpCell;

impl Guest for McpCell {
    fn run() -> Result<(), ()> {
        run_impl();
        Ok(())
    }
}

wasip2::cli::command::export!(McpCell);

fn run_impl() {
    let env = Rc::new(RefCell::new(Env::new()));
    let ctx = Rc::new(RefCell::new(McpSession {
        host: None,
        routing: None,
    }));
    let dispatch_table = Rc::new(build_dispatch());

    // Connect to the membrane via the WIT streams connection (not stdin/stdout).
    // This establishes a Cap'n Proto RPC channel to the host for cap grafting.
    // Meanwhile, WASI stdin/stdout remain free for JSON-RPC I/O.
    system::run(|membrane: Membrane| {
        let env = Rc::clone(&env);
        let ctx = Rc::clone(&ctx);
        let dispatch_table = Rc::clone(&dispatch_table);

        async move {
            // 1. Graft the membrane to obtain capabilities.
            let graft_resp = membrane.graft_request().send().promise.await?;
            let results = graft_resp.get()?;
            let caps = results.get_caps()?;
            let host: system_capnp::host::Client = get_graft_cap(&caps, "host")?;
            let routing: routing_capnp::routing::Client = get_graft_cap(&caps, "routing")?;

            // Populate session.
            {
                let mut s = ctx.borrow_mut();
                s.host = Some(host.clone());
                s.routing = Some(routing.clone());
            }

            // 2. Bind cap values + effect handlers into the environment.
            //    Uses shared handler factories from the caps crate.
            {
                let mut e = env.borrow_mut();
                let cap_handlers: [(&str, Val); 4] = [
                    ("host", make_host_handler(host)),
                    ("ipfs", make_ipfs_handler()),
                    ("routing", make_routing_handler(routing)),
                    ("import", make_import_handler()),
                ];
                for (name, handler) in cap_handlers {
                    e.set(name.to_string(), Val::Nil);
                    e.set(format!("{name}-handler"), handler);
                }
            }

            // 3. Load the prelude (macro definitions).
            {
                let mut e = env.borrow_mut();
                let prelude_forms = glia::read_many(glia::PRELUDE).expect("prelude: parse error");
                struct NoopDispatch;
                impl Dispatch for NoopDispatch {
                    fn call<'a>(
                        &'a self,
                        name: &'a str,
                        _args: &'a [Val],
                    ) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>> {
                        Box::pin(std::future::ready(Err(Val::from(format!(
                            "{name}: not available"
                        )))))
                    }
                }
                let noop = NoopDispatch;
                for form in &prelude_forms {
                    let mut fut = Box::pin(eval::eval_toplevel(form, &mut e, &noop));
                    let waker = std::task::Waker::noop();
                    let mut cx = std::task::Context::from_waker(&waker);
                    match fut.as_mut().poll(&mut cx) {
                        std::task::Poll::Ready(Ok(_)) => {}
                        std::task::Poll::Ready(Err(e)) => log::error!("prelude: {e}"),
                        std::task::Poll::Pending => log::error!("prelude: unexpected pending"),
                    }
                }
            }

            // 4. JSON-RPC loop on stdin/stdout.
            let stdin = std::io::stdin();
            let reader = stdin.lock();
            for line in reader.lines() {
                match line {
                    Ok(l) if l.trim().is_empty() => continue,
                    Ok(l) => {
                        let cont = handle_request(&l, &env, &ctx, &dispatch_table).await;
                        if !cont {
                            break;
                        }
                    }
                    Err(_) => break, // stdin EOF or error
                }
            }

            // Clean exit on stdin EOF.
            Ok(())
        }
    });
}
