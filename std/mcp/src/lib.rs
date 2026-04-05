//! MCP cell — JSON-RPC server for AI agent integration.
//!
//! A raw cell that speaks MCP (Model Context Protocol) over WASI
//! stdin/stdout.  Grafts the membrane to obtain capabilities, sets
//! up a Glia evaluator, and serves JSON-RPC requests.
//!
//! Per-capability tools make Glia's eval surface legible to AI agents.
//! `eval` remains the primary interface; per-cap tools are the discovery
//! layer.  Each tool call translates to a Glia expression internally.
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

// ---------------------------------------------------------------------------
// Dynamic tool generation from membrane capabilities
// ---------------------------------------------------------------------------

/// Build tool definitions for known capabilities.  Returns per-action schemas
/// that teach MCP clients what each capability can do.
fn tool_def_for_cap(name: &str) -> Option<serde_json::Value> {
    match name {
        "host" => Some(serde_json::json!({
            "name": "host",
            "description": "Node identity and peer management. Actions: id (peer identity), peers (connected peers), addrs (listen addresses).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["id", "peers", "addrs"],
                        "description": "The operation to perform"
                    }
                },
                "required": ["action"]
            }
        })),
        "routing" => Some(serde_json::json!({
            "name": "routing",
            "description": "DHT content routing (Kademlia). Actions: provide (announce a CID), find_providers (find peers hosting a CID).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["provide", "find_providers"],
                        "description": "The routing operation"
                    },
                    "cid": {
                        "type": "string",
                        "description": "Content identifier (CID)"
                    }
                },
                "required": ["action", "cid"]
            }
        })),
        "runtime" => Some(serde_json::json!({
            "name": "runtime",
            "description": "Load and execute WASM binaries as cells.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["run"],
                        "description": "The runtime operation"
                    },
                    "wasm_path": {
                        "type": "string",
                        "description": "Path to WASM binary in the FHS image (e.g. bin/myapp.wasm)"
                    }
                },
                "required": ["action", "wasm_path"]
            }
        })),
        "identity" => Some(serde_json::json!({
            "name": "identity",
            "description": "Ed25519 cryptographic operations. Actions: sign (sign a nonce with a domain-scoped key), verify (verify a signature against a public key).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["sign", "verify"],
                        "description": "The cryptographic operation"
                    },
                    "domain": {
                        "type": "string",
                        "description": "Signing domain (for sign action)"
                    },
                    "nonce": {
                        "type": "integer",
                        "description": "Nonce to sign (for sign action)"
                    },
                    "data": {
                        "type": "string",
                        "description": "Hex-encoded data to verify (for verify action)"
                    },
                    "signature": {
                        "type": "string",
                        "description": "Hex-encoded signature (for verify action)"
                    },
                    "pubkey": {
                        "type": "string",
                        "description": "Hex-encoded Ed25519 public key (for verify action)"
                    }
                },
                "required": ["action"]
            }
        })),
        "http-client" => Some(serde_json::json!({
            "name": "http-client",
            "description": "Outbound HTTP requests. Actions: get (HTTP GET), post (HTTP POST with body).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["get", "post"],
                        "description": "HTTP method"
                    },
                    "url": {
                        "type": "string",
                        "description": "Request URL"
                    },
                    "body": {
                        "type": "string",
                        "description": "Request body (for post action)"
                    }
                },
                "required": ["action", "url"]
            }
        })),
        "import" => Some(serde_json::json!({
            "name": "import",
            "description": "Load a Glia module by path. Returns the module's exported bindings.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Module path (e.g. 'core' resolves to /lib/core.glia)"
                    }
                },
                "required": ["path"]
            }
        })),
        _ => None,
    }
}

/// The eval tool — always present as the primary power interface.
fn eval_tool_def() -> serde_json::Value {
    serde_json::json!({
        "name": "eval",
        "description": "Evaluate a Glia s-expression on the Wetware node. This is the primary interface — use for complex operations, scripting, multi-step workflows, and anything not covered by other tools. Examples: (perform host :id), (def x (perform host :peers)), (help)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "Glia s-expression to evaluate"
                }
            },
            "required": ["expression"]
        }
    })
}

/// Build the tools list from the grafted capabilities.
///
/// Only known capabilities get per-cap tools.  Unknown caps are
/// accessible through `eval` — we don't generate tools from
/// untrusted capability names.
fn build_tools_list(cap_names: &[String]) -> serde_json::Value {
    let mut tools: Vec<serde_json::Value> = Vec::new();

    for name in cap_names {
        if let Some(def) = tool_def_for_cap(name) {
            tools.push(def);
        }
        // Unknown caps: no tool.  Use eval to interact.
    }

    // eval is always last — the primary power interface.
    tools.push(eval_tool_def());

    serde_json::json!({ "tools": tools })
}

/// Escape a string for safe embedding inside a Glia double-quoted string literal.
/// Prevents injection by escaping `"` and `\`.
fn glia_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Validate that a name contains only safe characters (alphanumeric, hyphens, underscores).
/// Rejects anything that could be used for Glia injection.
fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Translate a per-cap tool call into a Glia expression for eval.
///
/// Only known capabilities are supported.  Unknown caps must use the `eval` tool
/// directly — we do not generate expressions from untrusted capability names.
fn tool_call_to_glia(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    // Reject actions that aren't safe identifiers (prevents injection via :action keywords).
    if !action.is_empty() && !is_safe_identifier(action) {
        return None;
    }

    match tool_name {
        "host" => match action {
            "id" | "peers" | "addrs" => Some(format!("(perform host :{action})")),
            _ => None,
        },
        "routing" => {
            let cid = glia_escape(args.get("cid").and_then(|v| v.as_str()).unwrap_or(""));
            match action {
                "provide" => Some(format!("(perform routing :provide (bytes \"{cid}\"))")),
                "find_providers" => {
                    Some(format!("(perform routing :find-providers (bytes \"{cid}\"))"))
                }
                _ => None,
            }
        }
        "runtime" => {
            let path = glia_escape(args.get("wasm_path").and_then(|v| v.as_str()).unwrap_or(""));
            match action {
                "run" => Some(format!("(perform runtime :run (load \"{path}\"))")),
                _ => None,
            }
        }
        "identity" => match action {
            "sign" => {
                let domain = glia_escape(args.get("domain").and_then(|v| v.as_str()).unwrap_or("default"));
                let nonce = args.get("nonce").and_then(|v| v.as_u64()).unwrap_or(0);
                Some(format!("(perform identity :sign \"{domain}\" {nonce})"))
            }
            "verify" => {
                let data = glia_escape(args.get("data").and_then(|v| v.as_str()).unwrap_or(""));
                let sig = glia_escape(args.get("signature").and_then(|v| v.as_str()).unwrap_or(""));
                let pk = glia_escape(args.get("pubkey").and_then(|v| v.as_str()).unwrap_or(""));
                Some(format!(
                    "(perform identity :verify (bytes \"{data}\") (bytes \"{sig}\") (bytes \"{pk}\"))"
                ))
            }
            _ => None,
        },
        "http-client" => {
            let url = glia_escape(args.get("url").and_then(|v| v.as_str()).unwrap_or(""));
            match action {
                "get" => Some(format!("(perform http-client :get \"{url}\")")),
                "post" => {
                    let body = glia_escape(args.get("body").and_then(|v| v.as_str()).unwrap_or(""));
                    Some(format!("(perform http-client :post \"{url}\" \"{body}\")"))
                }
                _ => None,
            }
        }
        "import" => {
            let path = glia_escape(args.get("path").and_then(|v| v.as_str()).unwrap_or(""));
            Some(format!("(def imported (perform import \"{path}\"))"))
        }
        // Unknown caps: no tool dispatch.  Use eval directly.
        _ => None,
    }
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
    tools_list: &serde_json::Value,
    cap_names: &[String],
) -> bool {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            let null_id = serde_json::Value::Null;
            write_error(&null_id, -32700, &format!("Parse error: {e}"));
            return true;
        }
    };

    let id = match req.id {
        Some(ref id) => id.clone(),
        None => return true,
    };

    match req.method.as_str() {
        "initialize" => {
            write_result(&id, initialize_result());
        }
        "ping" => {
            write_result(&id, serde_json::json!({}));
        }
        "tools/list" => {
            write_result(&id, tools_list.clone());
        }
        "tools/call" => {
            let params = req.params.unwrap_or(serde_json::Value::Null);
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            if tool_name == "eval" {
                // Direct eval path.
                let expression = arguments
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if expression.is_empty() {
                    write_tool_error(&id, "empty expression");
                } else {
                    match eval_expression(expression, env, ctx, dispatch_table).await {
                        Ok(result) => write_tool_result(&id, &result),
                        Err(err) => write_tool_error(&id, &err),
                    }
                }
            } else if cap_names.iter().any(|n| n == tool_name) || tool_def_for_cap(tool_name).is_some() {
                // Per-cap tool: translate to Glia expression and eval.
                match tool_call_to_glia(tool_name, &arguments) {
                    Some(expr) => {
                        match eval_expression(&expr, env, ctx, dispatch_table).await {
                            Ok(result) => write_tool_result(&id, &result),
                            Err(err) => {
                                // Structured error for AI clients.
                                let action = arguments.get("action")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                write_tool_error(
                                    &id,
                                    &format!("{err}\n\nhint: capability '{tool_name}' action '{action}' failed. Try: (perform {tool_name} :{action})"),
                                );
                            }
                        }
                    }
                    None => {
                        let action = arguments.get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(none)");
                        write_tool_error(
                            &id,
                            &format!("Unknown action '{action}' for capability '{tool_name}'"),
                        );
                    }
                }
            } else {
                write_error(&id, -32602, &format!("Unknown tool: {tool_name}"));
            }
        }
        _ => {
            write_error(&id, -32601, &format!("Method not found: {}", req.method));
        }
    }

    true
}

/// Write a successful tool call result.
fn write_tool_result(id: &serde_json::Value, text: &str) {
    write_result(
        id,
        serde_json::json!({
            "content": [{"type": "text", "text": text}],
        }),
    );
}

/// Write a tool call error result.
fn write_tool_error(id: &serde_json::Value, message: &str) {
    write_result(
        id,
        serde_json::json!({
            "content": [{"type": "text", "text": message}],
            "isError": true,
        }),
    );
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

            // Extract capability names from the graft for dynamic tool generation.
            let cap_names: Vec<String> = (0..caps.len())
                .filter_map(|i| caps.get(i).get_name().ok().map(|s| s.to_string().ok()).flatten())
                .collect();

            let host: system_capnp::host::Client = get_graft_cap(&caps, "host")?;
            let routing: routing_capnp::routing::Client = get_graft_cap(&caps, "routing")?;

            // Build the dynamic tools list from grafted capabilities.
            let tools_list = build_tools_list(&cap_names);

            // Populate session.
            {
                let mut s = ctx.borrow_mut();
                s.host = Some(host.clone());
                s.routing = Some(routing.clone());
            }

            // 2. Bind cap values + effect handlers into the environment.
            //    Uses shared handler factories from the caps crate.
            //    Each cap must be Val::Cap (not Val::Nil) so that
            //    with-effect-handler can match on it.
            {
                let mut e = env.borrow_mut();
                let cap_handlers: [(&str, Val); 4] = [
                    ("host", make_host_handler(host)),
                    ("ipfs", make_ipfs_handler()),
                    ("routing", make_routing_handler(routing)),
                    ("import", make_import_handler()),
                ];
                for (name, handler) in cap_handlers {
                    // Each cap needs a unique schema_cid so that
                    // with-effect-handler can distinguish them.
                    // Use the cap name as a placeholder CID until
                    // real schema CIDs are wired through the graft.
                    e.set(
                        name.to_string(),
                        Val::Cap {
                            name: name.into(),
                            schema_cid: format!("mcp:{name}"),
                            inner: std::rc::Rc::new(()),
                        },
                    );
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
                        let cont = handle_request(
                            &l, &env, &ctx, &dispatch_table,
                            &tools_list, &cap_names,
                        ).await;
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
