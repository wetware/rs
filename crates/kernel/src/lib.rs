use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use glia::eval::{self, Dispatch, Env};
use glia::{read, read_many, Val};

use std::rc::Rc;

use wasip2::cli::stderr::get_stderr;
use wasip2::cli::stdin::get_stdin;
use wasip2::cli::stdout::get_stdout;
use wasip2::exports::cli::run::Guest;

#[allow(dead_code)]
mod system_capnp {
    include!(concat!(env!("OUT_DIR"), "/system_capnp.rs"));
}

#[allow(dead_code, clippy::extra_unused_type_parameters)]
mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/stem_capnp.rs"));
}

#[allow(dead_code, clippy::match_single_binding)]
mod ipfs_capnp {
    include!(concat!(env!("OUT_DIR"), "/ipfs_capnp.rs"));
}

#[allow(dead_code)]
mod routing_capnp {
    include!(concat!(env!("OUT_DIR"), "/routing_capnp.rs"));
}

#[allow(dead_code)]
mod http_capnp {
    include!(concat!(env!("OUT_DIR"), "/http_capnp.rs"));
}

// Content-addressed schema CIDs for built-in capability interfaces.
#[allow(dead_code)]
mod schema_ids {
    include!(concat!(env!("OUT_DIR"), "/schema_ids.rs"));
}

/// Bootstrap capability: the concrete Membrane defined in stem.capnp.
type Membrane = stem_capnp::membrane::Client;

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Trace
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let stderr = get_stderr();
        let _ = stderr.blocking_write_and_flush(
            format!("[{}] {}\n", record.level(), record.args()).as_bytes(),
        );
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn init_logging() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

// ---------------------------------------------------------------------------
// WASM custom section helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Evaluator — dispatches (capability method args...) to RPC calls
// ---------------------------------------------------------------------------

struct Session {
    host: system_capnp::host::Client,
    executor: system_capnp::executor::Client,
    routing: routing_capnp::routing::Client,
    /// Host-side node identity hub for this session.
    ///
    /// Call `identity.signer("ww-membrane-graft")` (or another known domain) to
    /// obtain a domain-scoped [`stem_capnp::signer::Client`].  The identity secret
    /// never crosses the host–guest boundary; only this capability reference is passed.
    #[allow(dead_code)]
    identity: stem_capnp::identity::Client,
    cwd: String,
}

/// Resolve the IPFS path for a relative reference.
///
/// If `path` starts with `/ipfs/`, it is returned as-is.
/// Otherwise, prepend `$WW_ROOT/` so that paths like `"bin/chess-demo.wasm"`
/// resolve to `<WW_ROOT>/bin/chess-demo.wasm`.
fn resolve_ipfs_path(path: &str) -> String {
    if path.starts_with("/ipfs/") {
        return path.to_string();
    }
    let root = std::env::var("WW_ROOT").unwrap_or_default();
    let root = root.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{root}/{path}")
}

// ---------------------------------------------------------------------------
// Dispatch table — builtins that don't go through the effect system
// ---------------------------------------------------------------------------

/// Async handler: takes evaluated args and the shell context.
type HandlerFn = for<'a> fn(
    &'a [Val],
    &'a RefCell<Session>,
) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>>;

/// Build the dispatch table for builtins only. Capability verbs (host, executor,
/// ipfs, routing) are handled via cap-targeted perform + with-effect-handler.
fn build_dispatch() -> HashMap<&'static str, HandlerFn> {
    let mut t: HashMap<&'static str, HandlerFn> = HashMap::new();
    t.insert("load", |a, _| Box::pin(std::future::ready(eval_load(a))));
    t.insert("cd", |a, c| Box::pin(std::future::ready(eval_cd(a, c))));
    t.insert("help", |_, _| {
        Box::pin(std::future::ready(Ok(Val::Str(HELP_TEXT.to_string()))))
    });
    t.insert("exit", |_, _| {
        Box::pin(std::future::ready({
            std::process::exit(0);
            #[allow(unreachable_code)]
            Ok(Val::Nil)
        }))
    });
    t
}

/// (load "path") — read bytes from the WASI virtual filesystem.
///
/// Relative paths like `"bin/chess-demo.wasm"` are resolved against the WASI
/// root (`/`), which the host preopens to the merged FHS image directory.
/// Absolute paths are used as-is.
///
/// Loaded files are cached in a thread-local map so repeated loads of the
/// same path (e.g. chess.glia loading the WASM for both :listen and :run)
/// return a cheap clone.  This also works around an ESPIPE (os error 29)
/// that occurs on the second `std::fs::read` in WASI P2 when the RPC
/// connection streams have been created between reads.
fn eval_load(args: &[Val]) -> Result<Val, Val> {
    thread_local! {
        static CACHE: RefCell<HashMap<String, Vec<u8>>> = RefCell::new(HashMap::new());
    }

    let path = match args.first() {
        Some(Val::Str(s)) => s.clone(),
        _ => return Err("(load \"<path>\")".into()),
    };
    // Resolve relative to WASI root — the host mounts the merged image at `/`.
    let resolved = if path.starts_with('/') {
        path.clone()
    } else {
        format!("/{path}")
    };

    // Return cached bytes if already loaded.
    let cached = CACHE.with(|c| c.borrow().get(&resolved).cloned());
    if let Some(bytes) = cached {
        return Ok(Val::Bytes(bytes));
    }

    let bytes =
        std::fs::read(&resolved).map_err(|e| Val::from(format!("load: {resolved}: {e}")))?;
    CACHE.with(|c| c.borrow_mut().insert(resolved, bytes.clone()));
    Ok(Val::Bytes(bytes))
}

fn eval_cd(args: &[Val], ctx: &RefCell<Session>) -> Result<Val, Val> {
    let path = match args.first() {
        Some(Val::Str(s)) => s.clone(),
        Some(Val::Sym(s)) => s.clone(),
        None => "/".to_string(),
        _ => return Err("(cd \"<path>\")".into()),
    };
    ctx.borrow_mut().cwd = path;
    Ok(Val::Nil)
}

// ---------------------------------------------------------------------------
// Kernel dispatch — bridges glia's evaluator to kernel capabilities
// ---------------------------------------------------------------------------

/// Bundles the capability context and dispatch table so the kernel can
/// implement [`glia::eval::Dispatch`].
struct KernelDispatch<'k> {
    ctx: &'k RefCell<Session>,
    table: &'k HashMap<&'static str, HandlerFn>,
}

impl<'k> Dispatch for KernelDispatch<'k> {
    fn call<'a>(
        &'a self,
        name: &'a str,
        args: &'a [Val],
    ) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>> {
        Box::pin(async move {
            match self.table.get(name) {
                Some(handler) => handler(args, self.ctx).await,
                None => eval_path_lookup(name, args, self.ctx).await,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Evaluator — delegates to glia with kernel dispatch
// ---------------------------------------------------------------------------

fn eval<'a>(
    expr: &'a Val,
    env: &'a mut Env,
    ctx: &'a RefCell<Session>,
    dispatch: &'a HashMap<&'static str, HandlerFn>,
) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>> {
    Box::pin(async move {
        let kd = KernelDispatch {
            ctx,
            table: dispatch,
        };
        eval::eval_toplevel(expr, env, &kd).await
    })
}

// ---------------------------------------------------------------------------
// Cap handlers — AsyncNativeFn closures that dispatch cap-targeted performs
// ---------------------------------------------------------------------------
//
// Each handler receives (data, resume) where data = (:method args...).
// The handler makes the RPC call and calls resume(result) to continue the body.
//
// Pattern: handler calls resume → returns Err(Val::Resume(val)) → poll_fn
// catches it → body future resumes with the value from the oneshot channel.

/// Call the resume function with a result value.
/// Returns the Resume sentinel that the poll_fn state machine expects.
fn call_resume(resume: &Val, val: Val) -> Result<Val, Val> {
    match resume {
        Val::NativeFn { func, .. } => func(&[val]),
        _ => Err(Val::from("cap handler: invalid resume function")),
    }
}

/// Extract (method_keyword, rest_args) from the data payload.
fn extract_method(data: &Val) -> Result<(String, Vec<Val>), Val> {
    let items = match data {
        Val::List(items) => items,
        _ => return Err(Val::from("cap handler: expected list data")),
    };
    let method = match items.first() {
        Some(Val::Keyword(s)) => s.clone(),
        _ => {
            return Err(Val::from(
                "cap handler: first arg must be a keyword method (e.g. :id, :run)",
            ))
        }
    };
    Ok((method, items[1..].to_vec()))
}

fn make_host_handler(host: system_capnp::host::Client) -> Val {
    Val::AsyncNativeFn {
        name: "host-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            let host = host.clone();
            Box::pin(async move {
                let (method, rest) = extract_method(&args[0])?;
                let resume = &args[1];
                let result = match method.as_str() {
                    "id" => {
                        let resp = host
                            .id_request()
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let id = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_peer_id()
                            .map_err(|e| Val::from(e.to_string()))?;
                        Val::Str(bs58::encode(id).into_string())
                    }
                    "addrs" => {
                        let resp = host
                            .addrs_request()
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let addrs = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_addrs()
                            .map_err(|e| Val::from(e.to_string()))?;
                        let items: Vec<Val> = (0..addrs.len())
                            .filter_map(|i| {
                                addrs
                                    .get(i)
                                    .ok()
                                    .and_then(|d| multiaddr::Multiaddr::try_from(d.to_vec()).ok())
                                    .map(|m| Val::Str(m.to_string()))
                            })
                            .collect();
                        Val::List(items)
                    }
                    "peers" => {
                        let resp = host
                            .peers_request()
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let peers = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_peers()
                            .map_err(|e| Val::from(e.to_string()))?;
                        let items: Vec<Val> = (0..peers.len())
                            .filter_map(|i| {
                                let peer = peers.get(i);
                                let id = peer
                                    .get_peer_id()
                                    .ok()
                                    .map(|b| bs58::encode(b).into_string())?;
                                let addrs = peer.get_addrs().ok()?;
                                let addr_vals: Vec<Val> = (0..addrs.len())
                                    .filter_map(|j| {
                                        addrs
                                            .get(j)
                                            .ok()
                                            .and_then(|a| {
                                                multiaddr::Multiaddr::try_from(a.to_vec()).ok()
                                            })
                                            .map(|m| Val::Str(m.to_string()))
                                    })
                                    .collect();
                                Some(Val::Map(vec![
                                    (Val::Keyword("peer-id".into()), Val::Str(id)),
                                    (Val::Keyword("addrs".into()), Val::List(addr_vals)),
                                ]))
                            })
                            .collect();
                        Val::List(items)
                    }
                    "listen" => {
                        // (perform host :listen executor <wasm>)         → VatListener
                        // (perform host :listen executor "proto" <wasm>) → StreamListener
                        let executor = match rest.first() {
                            Some(Val::Cap { name, inner, .. }) if name == "executor" => inner
                                .downcast_ref::<system_capnp::executor::Client>()
                                .cloned()
                                .ok_or_else(|| {
                                    Val::from("host :listen — executor cap has wrong inner type")
                                })?,
                            Some(Val::Cap { name, .. }) => {
                                return Err(Val::from(format!(
                                    "host :listen — expected executor cap, got '{name}'"
                                )))
                            }
                            _ => {
                                return Err(Val::from(
                                    "host :listen — executor capability required",
                                ))
                            }
                        };
                        match rest.len() {
                            2 => {
                                // VatListener mode: (perform host :listen executor <wasm> <schema>)
                                // Bind executor + wasm → BoundExecutor, then call
                                // VatListener.listen() with the explicit schema bytes.
                                let wasm = match rest.get(1) {
                                    Some(Val::Bytes(b)) => b.clone(),
                                    _ => {
                                        return Err(Val::from(
                                            "host :listen — expected wasm bytes as 2nd arg",
                                        ))
                                    }
                                };
                                let schema_bytes = match rest.get(2) {
                                    Some(Val::Bytes(b)) => b.clone(),
                                    _ => {
                                        return Err(Val::from(
                                            "host :listen — expected schema bytes as 3rd arg",
                                        ))
                                    }
                                };

                                // Bind the executor with the wasm to get a BoundExecutor.
                                // WW_CELL_MODE=vat is informational (guest dispatches on args).
                                let mut bind_req = executor.bind_request();
                                {
                                    let mut b = bind_req.get();
                                    b.set_wasm(&wasm);
                                    let mut env = b.init_env(1);
                                    env.set(0, "WW_CELL_MODE=vat");
                                }
                                let bind_resp = bind_req
                                    .send()
                                    .promise
                                    .await
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let bound = bind_resp
                                    .get()
                                    .map_err(|e| Val::from(e.to_string()))?
                                    .get_bound()
                                    .map_err(|e| Val::from(e.to_string()))?;

                                let network_resp = host
                                    .network_request()
                                    .send()
                                    .promise
                                    .await
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let network = network_resp
                                    .get()
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let listener = network
                                    .get_vat_listener()
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let mut req = listener.listen_request();
                                {
                                    let mut handler = req.get().init_handler();
                                    handler.set_spawn(bound);
                                }
                                req.get().set_schema(&schema_bytes);
                                req.send()
                                    .promise
                                    .await
                                    .map_err(|e| Val::from(e.to_string()))?;
                                log::info!("host :listen — registered vat handler");
                                Val::Nil
                            }
                            3 => {
                                // StreamListener mode: (perform host :listen executor "proto" <wasm>)
                                // Bind executor + wasm → BoundExecutor, then call
                                // StreamListener.listen() with BoundExecutor + protocol.
                                let protocol = match rest.get(1) {
                                    Some(Val::Str(s)) => s.clone(),
                                    _ => {
                                        return Err(Val::from(
                                            "host :listen — protocol must be a string",
                                        ))
                                    }
                                };
                                let wasm = match rest.get(2) {
                                    Some(Val::Bytes(b)) => b.clone(),
                                    _ => {
                                        return Err(Val::from(
                                            "host :listen — expected wasm bytes",
                                        ))
                                    }
                                };

                                // Bind the executor with the wasm to get a BoundExecutor.
                                // WW_CELL_MODE=raw is informational (guest dispatches on args).
                                let mut bind_req = executor.bind_request();
                                {
                                    let mut b = bind_req.get();
                                    b.set_wasm(&wasm);
                                    let mut env = b.init_env(1);
                                    env.set(0, "WW_CELL_MODE=raw");
                                }
                                let bind_resp = bind_req
                                    .send()
                                    .promise
                                    .await
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let bound = bind_resp
                                    .get()
                                    .map_err(|e| Val::from(e.to_string()))?
                                    .get_bound()
                                    .map_err(|e| Val::from(e.to_string()))?;

                                let network_resp = host
                                    .network_request()
                                    .send()
                                    .promise
                                    .await
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let network = network_resp
                                    .get()
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let listener = network
                                    .get_stream_listener()
                                    .map_err(|e| Val::from(e.to_string()))?;
                                let mut req = listener.listen_request();
                                req.get().set_executor(bound);
                                req.get().set_protocol(&protocol);
                                req.send()
                                    .promise
                                    .await
                                    .map_err(|e| Val::from(e.to_string()))?;
                                log::info!(
                                    "host :listen — registered stream handler /ww/0.1.0/stream/{protocol}"
                                );
                                Val::Nil
                            }
                            _ => {
                                return Err(Val::from(
                                    "host :listen — usage: (perform host :listen executor <wasm>) or (perform host :listen executor \"proto\" <wasm>)",
                                ))
                            }
                        }
                    }
                    _ => return Err(Val::from(format!("host: unknown method :{method}"))),
                };
                call_resume(resume, result)
            })
        }),
    }
}

fn make_executor_handler(executor: system_capnp::executor::Client) -> Val {
    Val::AsyncNativeFn {
        name: "executor-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            let executor = executor.clone();
            Box::pin(async move {
                let (method, rest) = extract_method(&args[0])?;
                let resume = &args[1];
                let result = match method.as_str() {
                    "echo" => {
                        let msg = match rest.first() {
                            Some(Val::Str(s)) | Some(Val::Sym(s)) => s.clone(),
                            _ => return Err(Val::from("executor :echo — expected string")),
                        };
                        let mut req = executor.echo_request();
                        req.get().set_message(&msg);
                        let resp = req
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let text = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_response()
                            .map_err(|e| Val::from(e.to_string()))?
                            .to_str()
                            .map_err(|e| Val::from(e.to_string()))?;
                        Val::Str(text.to_string())
                    }
                    "run" => {
                        // (perform executor :run <wasm-bytes> :env {"KEY" "VAL" ...})
                        let wasm = match rest.first() {
                            Some(Val::Bytes(b)) => b.clone(),
                            _ => {
                                return Err(Val::from(
                                    "executor :run — first arg must be wasm bytes",
                                ))
                            }
                        };
                        // Parse optional keyword args: :env {map}
                        let mut env_pairs: Vec<String> = Vec::new();
                        let mut i = 1;
                        while i < rest.len() {
                            if let Val::Keyword(k) = &rest[i] {
                                if k == "env" {
                                    i += 1;
                                    if let Some(Val::Map(pairs)) = rest.get(i) {
                                        for (k, v) in pairs {
                                            let key = match k {
                                                Val::Str(s) | Val::Sym(s) => s.clone(),
                                                other => format!("{other}"),
                                            };
                                            let val = match v {
                                                Val::Str(s) | Val::Sym(s) => s.clone(),
                                                other => format!("{other}"),
                                            };
                                            env_pairs.push(format!("{key}={val}"));
                                        }
                                    }
                                }
                            }
                            i += 1;
                        }

                        log::info!(
                            "executor :run — spawning process ({} bytes, {} env vars)",
                            wasm.len(),
                            env_pairs.len()
                        );

                        let mut req = executor.run_bytes_request();
                        {
                            let mut b = req.get();
                            b.set_wasm(&wasm);
                            if !env_pairs.is_empty() {
                                let mut env_list = b.init_env(env_pairs.len() as u32);
                                for (j, e) in env_pairs.iter().enumerate() {
                                    env_list.set(j as u32, e);
                                }
                            }
                        }
                        let resp = req
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let process = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_process()
                            .map_err(|e| Val::from(e.to_string()))?;

                        log::info!("executor :run — process spawned, waiting for exit");
                        let wait_resp = process
                            .wait_request()
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let exit_code = wait_resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_exit_code();
                        log::info!("executor :run — process exited ({})", exit_code);
                        Val::Int(exit_code as i64)
                    }
                    _ => return Err(Val::from(format!("executor: unknown method :{method}"))),
                };
                call_resume(resume, result)
            })
        }),
    }
}

/// Build the `ipfs` effect handler that reads content through the WASI virtual FS.
///
/// All IPFS content access goes through the WASI filesystem — the host-side CidTree
/// resolves paths lazily through the IPFS DAG. `cat` reads files, `ls` lists dirs.
/// `add` is not supported (content publishing goes through the stem contract).
fn make_ipfs_handler() -> Val {
    Val::AsyncNativeFn {
        name: "ipfs-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            Box::pin(async move {
                let (method, rest) = extract_method(&args[0])?;
                let resume = &args[1];
                let result = match method.as_str() {
                    "cat" => {
                        let raw_path = match rest.first() {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(Val::from("ipfs :cat — expected string path")),
                        };
                        let path = resolve_ipfs_path(&raw_path);
                        let data = std::fs::read(&path)
                            .map_err(|e| Val::from(format!("ipfs :cat {path}: {e}")))?;
                        Val::Bytes(data)
                    }
                    "ls" => {
                        let raw_path = match rest.first() {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(Val::from("ipfs :ls — expected string path")),
                        };
                        let path = resolve_ipfs_path(&raw_path);
                        let entries = std::fs::read_dir(&path)
                            .map_err(|e| Val::from(format!("ipfs :ls {path}: {e}")))?;
                        let items: Vec<Val> = entries
                            .filter_map(|entry| {
                                let entry = entry.ok()?;
                                let name = entry.file_name().to_str()?.to_string();
                                let size = entry.metadata().ok()?.len();
                                Some(Val::List(vec![Val::Str(name), Val::Sym(size.to_string())]))
                            })
                            .collect();
                        Val::List(items)
                    }
                    "add" => {
                        return Err(Val::from(
                            "ipfs :add is no longer supported — content publishing goes through the stem contract",
                        ));
                    }
                    _ => return Err(Val::from(format!("ipfs: unknown method :{method}"))),
                };
                call_resume(resume, result)
            })
        }),
    }
}

/// ProviderSink that collects streamed results into a channel.
struct CollectorSink {
    tx: std::sync::mpsc::Sender<(Vec<u8>, Vec<Vec<u8>>)>,
}

impl routing_capnp::provider_sink::Server for CollectorSink {
    async fn provider(
        self: capnp::capability::Rc<Self>,
        params: routing_capnp::provider_sink::ProviderParams,
    ) -> Result<(), capnp::Error> {
        let reader = params.get()?;
        let info = reader.get_info()?;
        let peer_id = info.get_peer_id()?.to_vec();
        let addrs_reader = info.get_addrs()?;
        let addrs: Vec<Vec<u8>> = (0..addrs_reader.len())
            .filter_map(|i| addrs_reader.get(i).ok().map(|a| a.to_vec()))
            .collect();
        let _ = self.tx.send((peer_id, addrs));
        Ok(())
    }

    async fn done(
        self: capnp::capability::Rc<Self>,
        _params: routing_capnp::provider_sink::DoneParams,
        _results: routing_capnp::provider_sink::DoneResults,
    ) -> Result<(), capnp::Error> {
        Ok(())
    }
}

/// Hash a name to a CID via routing.hash() RPC.
async fn routing_hash(routing: &routing_capnp::routing::Client, name: &str) -> Result<String, Val> {
    let mut req = routing.hash_request();
    req.get().set_data(name.as_bytes());
    let resp = req
        .send()
        .promise
        .await
        .map_err(|e| Val::from(e.to_string()))?;
    resp.get()
        .map_err(|e| Val::from(e.to_string()))?
        .get_key()
        .map_err(|e| Val::from(e.to_string()))?
        .to_str()
        .map(|s| s.to_string())
        .map_err(|e| Val::from(e.to_string()))
}

fn make_routing_handler(routing: routing_capnp::routing::Client) -> Val {
    Val::AsyncNativeFn {
        name: "routing-handler".into(),
        func: Rc::new(move |args: Vec<Val>| {
            let routing = routing.clone();
            Box::pin(async move {
                let (method, rest) = extract_method(&args[0])?;
                let resume = &args[1];
                let result = match method.as_str() {
                    "provide" => {
                        let name = match rest.first() {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(Val::from("routing :provide — expected string")),
                        };
                        let cid = routing_hash(&routing, &name).await?;
                        let mut req = routing.provide_request();
                        req.get().set_key(&cid);
                        req.send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        Val::Nil
                    }
                    "find" => {
                        let name = match rest.first() {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(Val::from("routing :find — expected string")),
                        };
                        // Parse optional :count keyword.
                        let mut count: u32 = 20;
                        let mut i = 1;
                        while i < rest.len() {
                            if let Val::Keyword(k) = &rest[i] {
                                if k == "count" {
                                    i += 1;
                                    if let Some(Val::Int(n)) = rest.get(i) {
                                        count = if *n <= 0 { u32::MAX } else { *n as u32 };
                                    }
                                }
                            }
                            i += 1;
                        }

                        let cid = routing_hash(&routing, &name).await?;

                        let (tx, rx) = std::sync::mpsc::channel();
                        let sink: routing_capnp::provider_sink::Client =
                            capnp_rpc::new_client(CollectorSink { tx });

                        let mut req = routing.find_providers_request();
                        req.get().set_key(&cid);
                        req.get().set_count(count);
                        req.get().set_sink(sink);
                        req.send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;

                        let mut providers = Vec::new();
                        while let Ok((peer_id, addrs)) = rx.try_recv() {
                            let id_str = bs58::encode(&peer_id).into_string();
                            let addr_vals: Vec<Val> = addrs
                                .into_iter()
                                .filter_map(|a| multiaddr::Multiaddr::try_from(a).ok())
                                .map(|m| Val::Str(m.to_string()))
                                .collect();
                            providers.push(Val::Map(vec![
                                (Val::Keyword("peer-id".into()), Val::Str(id_str)),
                                (Val::Keyword("addrs".into()), Val::List(addr_vals)),
                            ]));
                        }
                        Val::List(providers)
                    }
                    "hash" => {
                        let data = match rest.first() {
                            Some(Val::Str(s)) => s.as_bytes().to_vec(),
                            Some(Val::Bytes(b)) => b.clone(),
                            _ => return Err(Val::from("routing :hash — expected string or bytes")),
                        };
                        let mut req = routing.hash_request();
                        req.get().set_data(&data);
                        let resp = req
                            .send()
                            .promise
                            .await
                            .map_err(|e| Val::from(e.to_string()))?;
                        let key = resp
                            .get()
                            .map_err(|e| Val::from(e.to_string()))?
                            .get_key()
                            .map_err(|e| Val::from(e.to_string()))?
                            .to_str()
                            .map_err(|e| Val::from(e.to_string()))?;
                        Val::Str(key.to_string())
                    }
                    _ => return Err(Val::from(format!("routing: unknown method :{method}"))),
                };
                call_resume(resume, result)
            })
        }),
    }
}

async fn eval_path_lookup(cmd: &str, args: &[Val], ctx: &RefCell<Session>) -> Result<Val, Val> {
    // Convert args to strings once — used for whichever candidate we find.
    let str_args: Vec<String> = args
        .iter()
        .map(|v| match v {
            Val::Str(s) | Val::Sym(s) => s.clone(),
            other => format!("{other}"),
        })
        .collect();

    let path_var = std::env::var("PATH").unwrap_or_else(|_| "/bin".to_string());
    for dir in path_var.split(':') {
        // Candidate 1: <dir>/<cmd>.wasm (flat binary)
        // Candidate 2: <dir>/<cmd>/main.wasm (image-style nested)
        let candidates = [
            format!("{dir}/{cmd}.wasm"),
            format!("{dir}/{cmd}/main.wasm"),
        ];
        let bytes = candidates.iter().find_map(|p| std::fs::read(p).ok());
        if let Some(bytes) = bytes {
            let mut req = ctx.borrow().executor.run_bytes_request();
            {
                let mut b = req.get();
                b.set_wasm(&bytes);
                let mut arg_list = b.init_args(str_args.len() as u32);
                for (i, a) in str_args.iter().enumerate() {
                    arg_list.set(i as u32, a);
                }
            }
            let resp = req
                .send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            let process = resp
                .get()
                .map_err(|e| Val::from(e.to_string()))?
                .get_process()
                .map_err(|e| Val::from(e.to_string()))?;

            // Read stdout to completion.
            let stdout_resp = process
                .stdout_request()
                .send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            let stdout_stream = stdout_resp
                .get()
                .map_err(|e| Val::from(e.to_string()))?
                .get_stream()
                .map_err(|e| Val::from(e.to_string()))?;

            let mut output = Vec::new();
            loop {
                let mut req = stdout_stream.read_request();
                req.get().set_max_bytes(65536);
                let resp = req
                    .send()
                    .promise
                    .await
                    .map_err(|e| Val::from(e.to_string()))?;
                let chunk = resp
                    .get()
                    .map_err(|e| Val::from(e.to_string()))?
                    .get_data()
                    .map_err(|e| Val::from(e.to_string()))?;
                if chunk.is_empty() {
                    break;
                }
                output.extend_from_slice(chunk);
            }

            // Wait for exit.
            let wait_resp = process
                .wait_request()
                .send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            let exit_code = wait_resp
                .get()
                .map_err(|e| Val::from(e.to_string()))?
                .get_exit_code();

            let out_str = String::from_utf8_lossy(&output).trim_end().to_string();
            if exit_code != 0 {
                return Err(Val::from(format!(
                    "{cmd}: exit code {exit_code}\n{out_str}"
                )));
            }
            return Ok(Val::Str(out_str));
        }
    }
    Err(Val::from(format!("{cmd}: command not found")))
}

const HELP_TEXT: &str = "\
Capabilities (via perform):
  (perform host :id)                         Peer ID
  (perform host :addrs)                      Listen addresses
  (perform host :peers)                      Connected peers
  (perform host :listen executor <wasm>)     Register RPC handler (schema in WASM)
  (perform host :listen executor \"p\" <wasm>) Register stream handler

  (perform executor :echo \"<msg>\")           Diagnostic echo
  (perform executor :run <wasm> :env {})     Spawn foreground process

  (perform ipfs :cat \"<path>\")               Fetch IPFS content (bytes)
  (perform ipfs :ls \"<path>\")                List IPFS directory

  (perform routing :provide \"<name>\")        Announce to DHT (hashes internally)
  (perform routing :find \"<name>\" :count N)  Discover providers (default 20)
  (perform routing :hash \"<data>\")           Hash data to CID

Effects:
  (perform :load \"<path>\")                   Load bytes from virtual filesystem

Built-ins:
  (load \"<path>\")                Load bytes (dispatch form)
  (cd \"<path>\")                  Change working directory
  (help)                         This message
  (exit)                         Quit

Unrecognized commands are looked up in PATH (default /bin).";

// ---------------------------------------------------------------------------
// Init.d — evaluate scripts from $WW_ROOT/etc/init.d/*.glia
// ---------------------------------------------------------------------------

/// Parse an init.d script from raw bytes. Returns `None` on error (logs details).
/// Extracted from `run_initd` for testability — the caller uses `None` to skip
/// the failed script and continue (SysV best-effort model).
fn parse_initd_script(name: &str, data: &[u8]) -> Option<Vec<Val>> {
    let content = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(e) => {
            log::error!("init.d: {name}: not valid UTF-8: {e}");
            return None;
        }
    };
    match read_many(content) {
        Ok(forms) => {
            log::info!("init.d: parsed {name} ({} form(s))", forms.len());
            Some(forms)
        }
        Err(e) => {
            log::error!("init.d: {name}: parse error: {e}");
            None
        }
    }
}

/// Wrap a form in cap handlers + keyword effect handlers.
///
/// Produces:
/// ```glia
/// (with-effect-handler host host-handler
///   (with-effect-handler executor executor-handler
///     (with-effect-handler ipfs ipfs-handler
///       (with-effect-handler routing routing-handler
///         (with-effect-handler :load (fn [path resume] (resume (load path)))
///           <form>)))))
/// ```
///
/// Cap handlers are looked up from the environment by name. Keyword effect
/// handlers wrap builtins that use the effect protocol.
fn wrap_with_handlers(form: &Val) -> Val {
    // Innermost: keyword effect handler for :load.
    let with_load = Val::List(vec![
        Val::Sym("with-effect-handler".into()),
        Val::Keyword("load".into()),
        Val::List(vec![
            Val::Sym("fn".into()),
            Val::Vector(vec![Val::Sym("path".into()), Val::Sym("resume".into())]),
            Val::List(vec![
                Val::Sym("resume".into()),
                Val::List(vec![Val::Sym("load".into()), Val::Sym("path".into())]),
            ]),
        ]),
        form.clone(),
    ]);

    // Wrap in cap handlers (innermost to outermost).
    let caps = ["routing", "ipfs", "executor", "host"];
    let mut wrapped = with_load;
    for cap_name in &caps {
        let handler_name = format!("{cap_name}-handler");
        wrapped = Val::List(vec![
            Val::Sym("with-effect-handler".into()),
            Val::Sym(cap_name.to_string()),
            Val::Sym(handler_name),
            wrapped,
        ]);
    }
    wrapped
}

/// Scan `$WW_ROOT/etc/init.d/*.glia` via IPFS UnixFS, parse and evaluate
/// each file as a glia script. Returns true if any expression blocked
/// (i.e. a foreground process ran to completion via `(executor run ...)`).
async fn run_initd(
    env: &mut Env,
    ctx: &RefCell<Session>,
    dispatch: &HashMap<&'static str, HandlerFn>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let ww_root = std::env::var("WW_ROOT").unwrap_or_default();
    if ww_root.is_empty() {
        log::debug!("init.d: WW_ROOT not set, skipping");
        return Ok(false);
    }
    let root = ww_root.trim_end_matches('/');

    // Read init.d scripts via WASI virtual filesystem.
    // The host-side CidTree resolves these paths lazily through the IPFS DAG.
    let initd_path = format!("{root}/etc/init.d");
    let entries = match std::fs::read_dir(&initd_path) {
        Ok(dir) => {
            let mut names: Vec<String> = dir
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let name = entry.file_name().to_str()?.to_string();
                    if name.ends_with(".glia") {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            names.sort(); // lexicographic order for SysV init
            names
        }
        Err(e) => {
            log::debug!("init.d: readdir {initd_path} failed ({e}), skipping");
            return Ok(false);
        }
    };

    if entries.is_empty() {
        log::info!("init.d: no scripts found");
        return Ok(false);
    }

    log::info!("init.d: found {} script(s)", entries.len());
    let mut blocked = false;

    // SysV init: execute each script in lexicographic order, best-effort.
    // On failure: log with full context, continue to next script.
    for name in &entries {
        let script_path = format!("{initd_path}/{name}");

        // Read the glia script via WASI FS — failure skips this script.
        let data = match std::fs::read(&script_path) {
            Ok(d) => d,
            Err(e) => {
                log::error!("init.d: {name}: read failed: {e}");
                continue;
            }
        };

        let forms = match parse_initd_script(name, &data) {
            Some(f) => f,
            None => continue, // SysV: skip failed script
        };

        for (i, form) in forms.iter().enumerate() {
            log::info!("init.d: {name}: evaluating form {}/{}", i + 1, forms.len());
            // Wrap each form in default effect handlers so init.d
            // scripts can use (perform :load ...) etc.
            let wrapped = wrap_with_handlers(form);
            match eval(&wrapped, env, ctx, dispatch).await {
                Ok(Val::Nil) => {}
                Ok(Val::Int(code)) => {
                    // An (executor run ...) that returned an exit code means
                    // a foreground process ran to completion.
                    log::info!("init.d: {name}: foreground process exited ({code})");
                    blocked = true;
                }
                Ok(result) => {
                    log::debug!("init.d: {name}: {result}");
                }
                Err(e) => {
                    log::error!("init.d: {name}: form {}: {e}", i + 1);
                }
            }
        }
    }

    Ok(blocked)
}

// ---------------------------------------------------------------------------
// Shell mode (TTY)
// ---------------------------------------------------------------------------

fn write_prompt(stdout: &wasip2::io::streams::OutputStream, cwd: &str) {
    let prompt = format!("{} ❯ ", cwd);
    let _ = stdout.blocking_write_and_flush(prompt.as_bytes());
}

async fn run_shell(
    env: &mut Env,
    ctx: RefCell<Session>,
    dispatch: &HashMap<&'static str, HandlerFn>,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = get_stdin();
    let stdout = get_stdout();
    let stderr = get_stderr();

    write_prompt(&stdout, &ctx.borrow().cwd);
    let mut buf: Vec<u8> = Vec::new();

    'outer: loop {
        match stdin.blocking_read(4096) {
            Ok(b) if b.is_empty() => break 'outer,
            Ok(b) => buf.extend_from_slice(&b),
            Err(_) => break 'outer,
        }

        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes = buf.drain(..=pos).collect::<Vec<_>>();
            let line = match std::str::from_utf8(&line_bytes) {
                Ok(s) => s.trim(),
                Err(_) => {
                    write_prompt(&stdout, &ctx.borrow().cwd);
                    continue;
                }
            };

            if line.is_empty() {
                write_prompt(&stdout, &ctx.borrow().cwd);
                continue;
            }

            match read(line) {
                Ok(expr) => {
                    let wrapped = wrap_with_handlers(&expr);
                    match eval(&wrapped, env, &ctx, dispatch).await {
                        Ok(Val::Nil) => {}
                        Ok(result) => {
                            let _ =
                                stdout.blocking_write_and_flush(format!("{result}\n").as_bytes());
                        }
                        Err(e) => {
                            let _ =
                                stderr.blocking_write_and_flush(format!("error: {e}\n").as_bytes());
                        }
                    }
                }
                Err(e) => {
                    let _ =
                        stderr.blocking_write_and_flush(format!("parse error: {e}\n").as_bytes());
                }
            }

            write_prompt(&stdout, &ctx.borrow().cwd);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Daemon mode (non-TTY) — block on stdin
// ---------------------------------------------------------------------------

async fn run_daemon() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = get_stdin();
    loop {
        match stdin.blocking_read(4096) {
            Ok(b) if b.is_empty() => break,
            Err(_) => break,
            Ok(_) => {} // Discard input in daemon mode.
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

struct Kernel;

impl Guest for Kernel {
    fn run() -> Result<(), ()> {
        run_impl();
        Ok(())
    }
}

fn run_impl() {
    init_logging();

    system::run(|membrane: Membrane| async move {
        let graft_resp = membrane.graft_request().send().promise.await?;
        let results = graft_resp.get()?;

        let ctx = RefCell::new(Session {
            host: results.get_host()?,
            executor: results.get_executor()?,
            routing: results.get_routing()?,
            identity: results.get_identity()?,
            cwd: "/".to_string(),
        });

        let dispatch = build_dispatch();
        let mut env = Env::new();

        // Bind capability values + their effect handlers in the environment.
        // Scripts use (perform cap :method args...) to invoke capabilities.
        // The with-effect-handler wrapping (in wrap_with_handlers) routes
        // performs to these handlers via the effect system.
        {
            let s = ctx.borrow();
            let caps: [(&str, &str, Rc<dyn std::any::Any>, Val); 4] = [
                (
                    "host",
                    schema_ids::HOST_CID,
                    Rc::new(s.host.clone()),
                    make_host_handler(s.host.clone()),
                ),
                (
                    "executor",
                    schema_ids::EXECUTOR_CID,
                    Rc::new(s.executor.clone()),
                    make_executor_handler(s.executor.clone()),
                ),
                (
                    // ipfs handler now reads through WASI virtual FS, not RPC.
                    // The capability value is a placeholder — all actual I/O
                    // goes through std::fs calls intercepted by the host CidTree.
                    "ipfs",
                    schema_ids::IPFS_CID,
                    Rc::new(()),
                    make_ipfs_handler(),
                ),
                (
                    "routing",
                    schema_ids::ROUTING_CID,
                    Rc::new(s.routing.clone()),
                    make_routing_handler(s.routing.clone()),
                ),
            ];
            for (name, cid, inner, handler) in caps {
                env.set(
                    name.to_string(),
                    Val::Cap {
                        name: name.into(),
                        schema_cid: cid.to_string(),
                        inner,
                    },
                );
                env.set(format!("{name}-handler"), handler);
            }
        }

        // Load the prelude (standard macros: when, and, or, defn, cond, not).
        {
            let mut kd = KernelDispatch {
                ctx: &ctx,
                table: &dispatch,
            };
            glia::load_prelude(&mut env, &mut kd).await;
        }

        // Run init.d scripts first. If a foreground process blocked
        // (e.g. `(executor run ...)` in the script), we're done.
        let blocked = run_initd(&mut env, &ctx, &dispatch)
            .await
            .unwrap_or_else(|e| {
                log::error!("init.d: {e}");
                false
            });

        if !blocked {
            let is_tty = std::env::var("WW_TTY").is_ok();
            let result = if is_tty {
                run_shell(&mut env, ctx, &dispatch).await
            } else {
                run_daemon().await
            };

            if let Err(e) = result {
                log::error!("kernel error: {e}");
            }
        }

        Ok(())
    });
}

wasip2::cli::command::export!(Kernel);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ipfs_path_absolute_passthrough() {
        let p = "/ipfs/QmXXX/bin/main.wasm";
        assert_eq!(resolve_ipfs_path(p), p);
    }

    #[test]
    fn resolve_ipfs_path_relative() {
        std::env::set_var("WW_ROOT", "/ipfs/QmABC");
        assert_eq!(
            resolve_ipfs_path("bin/chess-demo.wasm"),
            "/ipfs/QmABC/bin/chess-demo.wasm"
        );
    }

    #[test]
    fn resolve_ipfs_path_trailing_slash_no_double() {
        std::env::set_var("WW_ROOT", "/ipfs/QmABC/");
        assert_eq!(
            resolve_ipfs_path("bin/chess-demo.wasm"),
            "/ipfs/QmABC/bin/chess-demo.wasm"
        );
    }

    #[test]
    fn resolve_ipfs_path_leading_slash_trimmed() {
        std::env::set_var("WW_ROOT", "/ipfs/QmABC");
        assert_eq!(
            resolve_ipfs_path("/bin/chess-demo.wasm"),
            "/ipfs/QmABC/bin/chess-demo.wasm"
        );
    }

    #[test]
    fn resolve_ipfs_path_empty_root() {
        std::env::remove_var("WW_ROOT");
        assert_eq!(
            resolve_ipfs_path("bin/chess-demo.wasm"),
            "/bin/chess-demo.wasm"
        );
    }

    // --- init.d parse + SysV error recovery ---

    #[test]
    fn parse_initd_script_valid() {
        let data = b"(cd \"/foo\") (cd \"/bar\")";
        let forms = parse_initd_script("test.glia", data).unwrap();
        assert_eq!(forms.len(), 2);
    }

    #[test]
    fn parse_initd_script_malformed() {
        let data = b"(cd \"/foo\") (broken";
        assert!(parse_initd_script("bad.glia", data).is_none());
    }

    #[test]
    fn parse_initd_script_invalid_utf8() {
        assert!(parse_initd_script("binary.glia", &[0xFF, 0xFE]).is_none());
    }

    #[test]
    fn parse_initd_script_empty() {
        let forms = parse_initd_script("empty.glia", b"").unwrap();
        assert!(forms.is_empty());
    }

    #[test]
    fn parse_initd_script_comments_only() {
        let data = b"; just a comment\n; another one\n";
        let forms = parse_initd_script("comments.glia", data).unwrap();
        assert!(forms.is_empty());
    }

    #[test]
    fn sysv_continues_past_failed_scripts() {
        // SysV contract: each script is processed independently.
        // parse_initd_script returns None on failure, enabling the caller
        // to `continue` to the next script.
        let scripts: Vec<(&str, &[u8])> = vec![
            ("01-bad.glia", &[0xFF, 0xFE]),           // invalid UTF-8
            ("02-broken.glia", b"(unclosed"),         // parse error
            ("03-good.glia", b"(cd \"/ok\")"),        // valid
            ("04-also-bad.glia", b"(a) )unexpected"), // parse error
            ("05-also-good.glia", b"(help)"),         // valid
        ];

        let results: Vec<Option<Vec<Val>>> = scripts
            .iter()
            .map(|(name, data)| parse_initd_script(name, data))
            .collect();

        assert!(results[0].is_none(), "invalid UTF-8 should fail");
        assert!(results[1].is_none(), "unclosed paren should fail");
        assert_eq!(
            results[2].as_ref().unwrap().len(),
            1,
            "valid script should parse"
        );
        assert!(results[3].is_none(), "unexpected close should fail");
        assert_eq!(
            results[4].as_ref().unwrap().len(),
            1,
            "valid script should parse"
        );
    }

    // --- load ---

    #[test]
    fn eval_load_missing_file_returns_error() {
        let result = eval_load(&[Val::Str("/nonexistent/path.wasm".into())]);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("No such file"), "got: {msg}");
    }

    #[test]
    fn eval_load_missing_arg_returns_error() {
        assert!(eval_load(&[]).is_err());
        assert!(eval_load(&[Val::Int(42)]).is_err());
    }

    #[test]
    fn eval_load_relative_path_prepends_slash() {
        // A relative path like "bin/foo.wasm" should resolve to "/bin/foo.wasm".
        let result = eval_load(&[Val::Str("nonexistent/relative.wasm".into())]);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("/nonexistent/relative.wasm"),
            "expected resolved path with leading /, got: {msg}"
        );
    }

    #[test]
    fn eval_load_caches_repeated_reads() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("cached.bin");
        std::fs::write(&file_path, b"cached-bytes").unwrap();

        let abs = file_path.to_str().unwrap().to_string();
        let first = eval_load(&[Val::Str(abs.clone())]);
        assert_eq!(first.unwrap(), Val::Bytes(b"cached-bytes".to_vec()));

        // Mutate the file on disk — cached result should still return old bytes.
        std::fs::write(&file_path, b"new-bytes").unwrap();
        let second = eval_load(&[Val::Str(abs)]);
        assert_eq!(second.unwrap(), Val::Bytes(b"cached-bytes".to_vec()));
    }

    // --- wrap_with_handlers ---

    #[test]
    fn wrap_with_handlers_nests_effect_handlers() {
        let form = Val::Sym("body".into());
        let wrapped = wrap_with_handlers(&form);
        // Outermost should be (with-effect-handler host host-handler ...)
        if let Val::List(items) = &wrapped {
            assert_eq!(items[0], Val::Sym("with-effect-handler".into()));
            assert_eq!(items[1], Val::Sym("host".into()));
            assert_eq!(items[2], Val::Sym("host-handler".into()));
            // items[3] is (with-effect-handler executor ...)
            if let Val::List(inner) = &items[3] {
                assert_eq!(inner[0], Val::Sym("with-effect-handler".into()));
                assert_eq!(inner[1], Val::Sym("executor".into()));
            } else {
                panic!("expected nested effect handler");
            }
        } else {
            panic!("expected List");
        }
    }

    // --- dispatch table ---

    #[test]
    fn dispatch_table_has_builtins() {
        let table = build_dispatch();
        let expected = ["load", "cd", "help", "exit"];
        for verb in &expected {
            assert!(table.contains_key(verb), "missing dispatch entry: {verb}");
        }
        assert_eq!(
            table.len(),
            expected.len(),
            "unexpected extra entries in dispatch table"
        );
    }

    // ===================================================================
    // Integration tests — dispatch handlers against capnp-rpc stub servers
    // ===================================================================

    use capnp::capability::Promise;

    // Fixed test data: a 38-byte multihash peer ID (identity hash of "test-peer").
    // bs58 of these bytes is "12D3KooW..." in real life; here we use a short
    // deterministic value so assertions are stable.
    const STUB_PEER_ID: &[u8] = b"test-peer-id-multihash-bytes-1234";
    // /ip4/127.0.0.1/tcp/4001 as multiaddr bytes
    const STUB_MULTIADDR: &[u8] = &[0x04, 127, 0, 0, 1, 0x06, 0x0f, 0xa1];

    // --- Stub Host: returns fixed peer ID, addrs, peers ---

    struct TestHost;

    #[allow(refining_impl_trait)]
    impl system_capnp::host::Server for TestHost {
        fn id(
            self: capnp::capability::Rc<Self>,
            _params: system_capnp::host::IdParams,
            mut results: system_capnp::host::IdResults,
        ) -> Promise<(), capnp::Error> {
            results.get().set_peer_id(STUB_PEER_ID);
            Promise::ok(())
        }

        fn addrs(
            self: capnp::capability::Rc<Self>,
            _params: system_capnp::host::AddrsParams,
            mut results: system_capnp::host::AddrsResults,
        ) -> Promise<(), capnp::Error> {
            let mut list = results.get().init_addrs(1);
            list.set(0, STUB_MULTIADDR);
            Promise::ok(())
        }

        fn peers(
            self: capnp::capability::Rc<Self>,
            _params: system_capnp::host::PeersParams,
            mut results: system_capnp::host::PeersResults,
        ) -> Promise<(), capnp::Error> {
            let mut list = results.get().init_peers(1);
            {
                let mut peer = list.reborrow().get(0);
                peer.set_peer_id(STUB_PEER_ID);
                let mut addrs = peer.init_addrs(1);
                addrs.set(0, STUB_MULTIADDR);
            }
            Promise::ok(())
        }

        fn executor(
            self: capnp::capability::Rc<Self>,
            _params: system_capnp::host::ExecutorParams,
            _results: system_capnp::host::ExecutorResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }

        fn network(
            self: capnp::capability::Rc<Self>,
            _params: system_capnp::host::NetworkParams,
            mut results: system_capnp::host::NetworkResults,
        ) -> Promise<(), capnp::Error> {
            let mut r = results.get();
            r.set_stream_listener(capnp_rpc::new_client(TestStreamListener));
            r.set_stream_dialer(capnp_rpc::new_client(TestStreamDialer));
            r.set_vat_listener(capnp_rpc::new_client(TestVatListener));
            r.set_vat_client(capnp_rpc::new_client(TestVatClient));
            Promise::ok(())
        }
    }

    // --- Stub Executor: echo returns input ---

    struct TestExecutor;

    #[allow(refining_impl_trait)]
    impl system_capnp::executor::Server for TestExecutor {
        fn echo(
            self: capnp::capability::Rc<Self>,
            params: system_capnp::executor::EchoParams,
            mut results: system_capnp::executor::EchoResults,
        ) -> Promise<(), capnp::Error> {
            let msg = capnp_rpc::pry!(capnp_rpc::pry!(params.get()).get_message());
            results.get().set_response(msg);
            Promise::ok(())
        }

        fn run_bytes(
            self: capnp::capability::Rc<Self>,
            _params: system_capnp::executor::RunBytesParams,
            _results: system_capnp::executor::RunBytesResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
    }

    // --- Stub Routing: hash returns fixed CID, provide succeeds, findProviders streams 2 results ---

    struct TestRouting;

    #[allow(refining_impl_trait)]
    impl routing_capnp::routing::Server for TestRouting {
        fn hash(
            self: capnp::capability::Rc<Self>,
            _params: routing_capnp::routing::HashParams,
            mut results: routing_capnp::routing::HashResults,
        ) -> Promise<(), capnp::Error> {
            results.get().set_key("QmTestCid123");
            Promise::ok(())
        }

        fn provide(
            self: capnp::capability::Rc<Self>,
            _params: routing_capnp::routing::ProvideParams,
            _results: routing_capnp::routing::ProvideResults,
        ) -> Promise<(), capnp::Error> {
            Promise::ok(())
        }

        fn find_providers(
            self: capnp::capability::Rc<Self>,
            params: routing_capnp::routing::FindProvidersParams,
            _results: routing_capnp::routing::FindProvidersResults,
        ) -> Promise<(), capnp::Error> {
            let params = capnp_rpc::pry!(params.get());
            let count = params.get_count();
            let sink = capnp_rpc::pry!(params.get_sink());

            // Stream `min(count, 2)` providers.
            let n = std::cmp::min(count, 2) as usize;
            Promise::from_future(async move {
                for i in 0..n {
                    let mut req = sink.provider_request();
                    {
                        let mut info = req.get().init_info();
                        info.set_peer_id(format!("peer-{i}").as_bytes());
                        let mut addrs = info.init_addrs(1);
                        addrs.set(0, STUB_MULTIADDR);
                    }
                    req.send().await?;
                }
                let done_req = sink.done_request();
                done_req.send().promise.await?;
                Ok(())
            })
        }
    }

    // --- Stub VatListener: asserts executor is present ---

    struct TestVatListener;

    #[allow(refining_impl_trait)]
    impl system_capnp::vat_listener::Server for TestVatListener {
        fn listen(
            self: capnp::capability::Rc<Self>,
            params: system_capnp::vat_listener::ListenParams,
            _results: system_capnp::vat_listener::ListenResults,
        ) -> Promise<(), capnp::Error> {
            let params = capnp_rpc::pry!(params.get());
            if !params.has_executor() {
                return Promise::err(capnp::Error::failed("executor not set".into()));
            }
            if !params.has_wasm() {
                return Promise::err(capnp::Error::failed("wasm not set".into()));
            }
            Promise::ok(())
        }
    }

    // --- Stub StreamListener: asserts executor is present ---

    struct TestStreamListener;

    #[allow(refining_impl_trait)]
    impl system_capnp::stream_listener::Server for TestStreamListener {
        fn listen(
            self: capnp::capability::Rc<Self>,
            params: system_capnp::stream_listener::ListenParams,
            _results: system_capnp::stream_listener::ListenResults,
        ) -> Promise<(), capnp::Error> {
            let params = capnp_rpc::pry!(params.get());
            if !params.has_executor() {
                return Promise::err(capnp::Error::failed("executor not set".into()));
            }
            if !params.has_protocol() {
                return Promise::err(capnp::Error::failed("protocol not set".into()));
            }
            if !params.has_wasm() {
                return Promise::err(capnp::Error::failed("wasm not set".into()));
            }
            Promise::ok(())
        }
    }

    // --- Stub StreamDialer + VatClient (unused, just satisfy network result) ---

    struct TestStreamDialer;
    impl system_capnp::stream_dialer::Server for TestStreamDialer {}

    struct TestVatClient;
    impl system_capnp::vat_client::Server for TestVatClient {}

    // --- Stub IPFS + Identity (unimplemented — not under test) ---

    struct TestIpfs;

    #[allow(refining_impl_trait)]
    impl ipfs_capnp::client::Server for TestIpfs {
        fn unixfs(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::UnixfsParams,
            _r: ipfs_capnp::client::UnixfsResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn block(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::BlockParams,
            _r: ipfs_capnp::client::BlockResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn dag(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::DagParams,
            _r: ipfs_capnp::client::DagResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn name(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::NameParams,
            _r: ipfs_capnp::client::NameResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn key(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::KeyParams,
            _r: ipfs_capnp::client::KeyResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn pin(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::PinParams,
            _r: ipfs_capnp::client::PinResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn object(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::ObjectParams,
            _r: ipfs_capnp::client::ObjectResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn swarm(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::SwarmParams,
            _r: ipfs_capnp::client::SwarmResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn pub_sub(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::PubSubParams,
            _r: ipfs_capnp::client::PubSubResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn routing(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::RoutingParams,
            _r: ipfs_capnp::client::RoutingResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn resolve_path(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::ResolvePathParams,
            _r: ipfs_capnp::client::ResolvePathResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
        fn resolve_node(
            self: capnp::capability::Rc<Self>,
            _p: ipfs_capnp::client::ResolveNodeParams,
            _r: ipfs_capnp::client::ResolveNodeResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
    }

    struct TestIdentity;

    #[allow(refining_impl_trait)]
    impl stem_capnp::identity::Server for TestIdentity {
        fn signer(
            self: capnp::capability::Rc<Self>,
            _p: stem_capnp::identity::SignerParams,
            _r: stem_capnp::identity::SignerResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("stub".into()))
        }
    }

    // --- Helper: construct a Session with test stubs ---

    fn test_session() -> Session {
        Session {
            host: capnp_rpc::new_client(TestHost),
            executor: capnp_rpc::new_client(TestExecutor),
            ipfs: capnp_rpc::new_client(TestIpfs),
            routing: capnp_rpc::new_client(TestRouting),
            identity: capnp_rpc::new_client(TestIdentity),
            cwd: "/".into(),
        }
    }

    /// Run an async block on a single-threaded tokio + capnp-rpc LocalSet.
    async fn run_local<F, T>(f: F) -> T
    where
        F: Future<Output = T>,
    {
        tokio::task::LocalSet::new().run_until(f).await
    }

    /// Call an AsyncNativeFn handler with a method keyword + rest args.
    /// Provides a resume function and extracts the resumed value.
    /// Returns Ok(resumed_value) or the handler's Err.
    async fn call_handler(handler: &Val, method: &str, rest: &[Val]) -> Result<Val, Val> {
        let func = match handler {
            Val::AsyncNativeFn { func, .. } => func.clone(),
            _ => panic!("expected AsyncNativeFn"),
        };
        let mut data_items = vec![Val::Keyword(method.into())];
        data_items.extend_from_slice(rest);
        let data = Val::List(data_items);

        // Create a resume function that captures the value.
        let captured: Rc<RefCell<Option<Val>>> = Rc::new(RefCell::new(None));
        let cap = captured.clone();
        let resume = Val::NativeFn {
            name: "test-resume".into(),
            func: Rc::new(move |args: &[Val]| {
                *cap.borrow_mut() = Some(args[0].clone());
                Err(Val::Resume(Box::new(args[0].clone())))
            }),
        };

        match func(vec![data, resume]).await {
            Err(Val::Resume(_)) => {
                // Handler called resume — extract the value.
                Ok(captured.borrow().clone().unwrap())
            }
            Err(e) => Err(e),
            Ok(v) => Ok(v), // Handler returned directly without resume.
        }
    }

    // --- host tests ---

    #[tokio::test]
    async fn test_host_id_returns_bs58() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let result = call_handler(&handler, "id", &[]).await.unwrap();
            let expected = bs58::encode(STUB_PEER_ID).into_string();
            assert_eq!(result, Val::Str(expected));
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_addrs_returns_multiaddr_strings() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let result = call_handler(&handler, "addrs", &[]).await.unwrap();
            match result {
                Val::List(addrs) => {
                    assert_eq!(addrs.len(), 1);
                    assert_eq!(addrs[0], Val::Str("/ip4/127.0.0.1/tcp/4001".into()));
                }
                other => panic!("expected list, got {other:?}"),
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_peers_returns_map_format() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let result = call_handler(&handler, "peers", &[]).await.unwrap();
            match result {
                Val::List(peers) => {
                    assert_eq!(peers.len(), 1);
                    match &peers[0] {
                        Val::Map(entries) => {
                            assert_eq!(entries.len(), 2);
                            assert_eq!(entries[0].0, Val::Keyword("peer-id".into()));
                            let expected_id = bs58::encode(STUB_PEER_ID).into_string();
                            assert_eq!(entries[0].1, Val::Str(expected_id));
                            assert_eq!(entries[1].0, Val::Keyword("addrs".into()));
                        }
                        other => panic!("expected map, got {other:?}"),
                    }
                }
                other => panic!("expected list, got {other:?}"),
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_unknown_method_returns_error() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let err = call_handler(&handler, "bogus", &[]).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("unknown method"), "got: {msg}");
        })
        .await;
    }

    // --- host listen tests ---

    #[tokio::test]
    async fn test_host_listen_vat_passes_executor() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let executor_cap = Val::Cap {
                name: "executor".into(),
                schema_cid: "test-executor-cid".into(),
                inner: Rc::new(s.executor.clone()),
            };
            let result = call_handler(
                &handler,
                "listen",
                &[executor_cap, Val::Bytes(b"fake-wasm".to_vec())],
            )
            .await;
            assert!(
                result.is_ok(),
                "VatListener listen failed: {:?}",
                result.unwrap_err()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_stream_passes_executor() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let executor_cap = Val::Cap {
                name: "executor".into(),
                schema_cid: "test-executor-cid".into(),
                inner: Rc::new(s.executor.clone()),
            };
            let result = call_handler(
                &handler,
                "listen",
                &[
                    executor_cap,
                    Val::Str("my-protocol".into()),
                    Val::Bytes(b"fake-wasm".to_vec()),
                ],
            )
            .await;
            assert!(
                result.is_ok(),
                "StreamListener listen failed: {:?}",
                result.unwrap_err()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_missing_executor_errors() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let err = call_handler(&handler, "listen", &[Val::Bytes(b"wasm".to_vec())]).await;
            assert!(err.is_err(), "should require executor capability");
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_wrong_cap_type_errors() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let bad_cap = Val::Cap {
                name: "not-executor".into(),
                schema_cid: "test-not-executor-cid".into(),
                inner: Rc::new(42i32),
            };
            let err =
                call_handler(&handler, "listen", &[bad_cap, Val::Bytes(b"wasm".to_vec())]).await;
            assert!(err.is_err(), "should reject wrong capability type");
            let msg = format!("{}", err.unwrap_err());
            assert!(
                msg.contains("not-executor"),
                "error should name the wrong cap: {msg}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_forged_executor_cap_wrong_inner_type() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let forged_cap = Val::Cap {
                name: "executor".into(),
                schema_cid: "test-executor-cid".into(),
                inner: Rc::new(42i32),
            };
            let err = call_handler(
                &handler,
                "listen",
                &[forged_cap, Val::Bytes(b"wasm".to_vec())],
            )
            .await;
            assert!(err.is_err(), "forged cap should be rejected");
            let msg = format!("{}", err.unwrap_err());
            assert!(msg.contains("wrong inner type"), "got: {msg}");
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_string_instead_of_cap_errors() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let err = call_handler(
                &handler,
                "listen",
                &[Val::Str("executor".into()), Val::Bytes(b"wasm".to_vec())],
            )
            .await;
            assert!(err.is_err(), "string should not pass as executor cap");
            let msg = format!("{}", err.unwrap_err());
            assert!(msg.contains("executor capability required"), "got: {msg}");
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_nil_instead_of_cap_errors() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let err = call_handler(
                &handler,
                "listen",
                &[Val::Nil, Val::Bytes(b"wasm".to_vec())],
            )
            .await;
            assert!(err.is_err(), "nil should not pass as executor cap");
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_stream_wrong_cap_type_errors() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let bad_cap = Val::Cap {
                name: "imposter".into(),
                schema_cid: "test-imposter-cid".into(),
                inner: Rc::new(42i32),
            };
            let err = call_handler(
                &handler,
                "listen",
                &[
                    bad_cap,
                    Val::Str("my-protocol".into()),
                    Val::Bytes(b"wasm".to_vec()),
                ],
            )
            .await;
            assert!(err.is_err(), "wrong cap should be rejected in stream mode");
            let msg = format!("{}", err.unwrap_err());
            assert!(
                msg.contains("imposter"),
                "error should name the wrong cap: {msg}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_stream_missing_executor_errors() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            let err = call_handler(
                &handler,
                "listen",
                &[Val::Str("my-protocol".into()), Val::Bytes(b"wasm".to_vec())],
            )
            .await;
            assert!(err.is_err(), "stream listen without executor should error");
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_wrong_arity_returns_error() {
        run_local(async {
            let s = test_session();
            let handler = make_host_handler(s.host.clone());
            // 0 args after :listen — should error
            assert!(call_handler(&handler, "listen", &[]).await.is_err());
            // 4 args after :listen — should error
            let executor_cap = Val::Cap {
                name: "executor".into(),
                schema_cid: "test-executor-cid".into(),
                inner: Rc::new(s.executor.clone()),
            };
            assert!(call_handler(
                &handler,
                "listen",
                &[
                    executor_cap,
                    Val::Str("a".into()),
                    Val::Bytes(b"b".to_vec()),
                    Val::Str("extra".into()),
                ],
            )
            .await
            .is_err());
        })
        .await;
    }

    // --- executor tests ---

    #[tokio::test]
    async fn test_executor_echo() {
        run_local(async {
            let s = test_session();
            let handler = make_executor_handler(s.executor.clone());
            let result = call_handler(&handler, "echo", &[Val::Str("hello".into())])
                .await
                .unwrap();
            assert_eq!(result, Val::Str("hello".into()));
        })
        .await;
    }

    // --- routing tests ---

    #[tokio::test]
    async fn test_routing_provide_succeeds() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let result = call_handler(&handler, "provide", &[Val::Str("oracle".into())])
                .await
                .unwrap();
            assert_eq!(result, Val::Nil);
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_provide_missing_name() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let err = call_handler(&handler, "provide", &[]).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("routing :provide"), "got: {msg}");
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_find_default_count() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let result = call_handler(&handler, "find", &[Val::Str("oracle".into())])
                .await
                .unwrap();
            match result {
                Val::List(providers) => {
                    assert_eq!(providers.len(), 2);
                    match &providers[0] {
                        Val::Map(entries) => {
                            assert_eq!(entries[0].0, Val::Keyword("peer-id".into()));
                            assert_eq!(
                                entries[0].1,
                                Val::Str(bs58::encode(b"peer-0").into_string())
                            );
                        }
                        other => panic!("expected map, got {other:?}"),
                    }
                }
                other => panic!("expected list, got {other:?}"),
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_find_custom_count() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let result = call_handler(
                &handler,
                "find",
                &[
                    Val::Str("oracle".into()),
                    Val::Keyword("count".into()),
                    Val::Int(1),
                ],
            )
            .await
            .unwrap();
            match result {
                Val::List(providers) => assert_eq!(providers.len(), 1),
                other => panic!("expected list, got {other:?}"),
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_find_zero_count_means_no_limit() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let result = call_handler(
                &handler,
                "find",
                &[
                    Val::Str("oracle".into()),
                    Val::Keyword("count".into()),
                    Val::Int(0),
                ],
            )
            .await
            .unwrap();
            match result {
                Val::List(providers) => assert_eq!(providers.len(), 2),
                other => panic!("expected list, got {other:?}"),
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_find_missing_name() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let err = call_handler(&handler, "find", &[]).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("routing :find"), "got: {msg}");
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_hash() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let result = call_handler(&handler, "hash", &[Val::Str("test-data".into())])
                .await
                .unwrap();
            assert_eq!(result, Val::Str("QmTestCid123".into()));
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_unknown_method_returns_error() {
        run_local(async {
            let s = test_session();
            let handler = make_routing_handler(s.routing.clone());
            let err = call_handler(&handler, "bogus", &[]).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("unknown method"), "got: {msg}");
        })
        .await;
    }

    // --- perform :load effect round-trip ---

    /// Helper: bind all caps + handlers in env (same as kernel boot).
    fn bind_caps_in_env(env: &mut Env, session: &Session) {
        let caps: [(&str, &str, Rc<dyn std::any::Any>, Val); 4] = [
            (
                "host",
                "test-host-cid",
                Rc::new(session.host.clone()),
                make_host_handler(session.host.clone()),
            ),
            (
                "executor",
                "test-executor-cid",
                Rc::new(session.executor.clone()),
                make_executor_handler(session.executor.clone()),
            ),
            (
                "ipfs",
                "test-ipfs-cid",
                Rc::new(session.ipfs.clone()),
                make_ipfs_handler(session.ipfs.clone()),
            ),
            (
                "routing",
                "test-routing-cid",
                Rc::new(session.routing.clone()),
                make_routing_handler(session.routing.clone()),
            ),
        ];
        for (name, cid, inner, handler) in caps {
            env.set(
                name.to_string(),
                Val::Cap {
                    name: name.into(),
                    schema_cid: cid.into(),
                    inner,
                },
            );
            env.set(format!("{name}-handler"), handler);
        }
    }

    /// Verify that (perform :load "path") inside wrap_with_handlers
    /// actually resolves through the effect handler → eval_load → filesystem.
    #[tokio::test]
    async fn test_perform_load_resolves_through_effect_handler() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();
            bind_caps_in_env(&mut env, &ctx.borrow());

            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("test.bin");
            std::fs::write(&file_path, b"hello-bytes").unwrap();
            std::env::remove_var("WW_ROOT");

            let form = Val::List(vec![
                Val::Sym("perform".into()),
                Val::Keyword("load".into()),
                Val::Str(file_path.to_str().unwrap().to_string()),
            ]);
            let wrapped = wrap_with_handlers(&form);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert_eq!(result.unwrap(), Val::Bytes(b"hello-bytes".to_vec()));
        })
        .await;
    }

    /// Verify that (perform :load "missing") fails with a clear error.
    #[tokio::test]
    async fn test_perform_load_missing_file_returns_error() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();
            bind_caps_in_env(&mut env, &ctx.borrow());

            std::env::remove_var("WW_ROOT");

            let form = Val::List(vec![
                Val::Sym("perform".into()),
                Val::Keyword("load".into()),
                Val::Str("/nonexistent/path/missing.wasm".to_string()),
            ]);
            let wrapped = wrap_with_handlers(&form);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert!(result.is_err(), "expected error for missing file");
        })
        .await;
    }

    // --- init script eval integration ---

    /// Eval (perform host :listen executor (perform :load "path")) end-to-end.
    #[tokio::test]
    async fn test_chess_glia_listen_form_evals_end_to_end() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();
            bind_caps_in_env(&mut env, &ctx.borrow());

            let dir = tempfile::tempdir().unwrap();
            let wasm_path = dir.path().join("chess-demo.wasm");
            std::fs::write(&wasm_path, b"fake-wasm-bytes").unwrap();
            std::env::remove_var("WW_ROOT");

            let script = format!(
                r#"(perform host :listen executor (perform :load "{}"))"#,
                wasm_path.to_str().unwrap()
            );
            let form = read(&script).unwrap();
            let wrapped = wrap_with_handlers(&form);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert!(
                result.is_ok(),
                "chess.glia listen form failed: {:?}",
                result.unwrap_err()
            );
        })
        .await;
    }

    /// Eval the full chess.glia script through the kernel eval pipeline.
    #[tokio::test]
    async fn test_chess_glia_full_script_parses_and_first_form_evals() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();
            bind_caps_in_env(&mut env, &ctx.borrow());

            let dir = tempfile::tempdir().unwrap();
            let wasm_path = dir.path().join("chess-demo.wasm");
            std::fs::write(&wasm_path, b"fake-wasm-bytes").unwrap();
            std::env::remove_var("WW_ROOT");

            let script = format!(
                r#"(perform host :listen executor (perform :load "{}"))
                   (perform executor :run (perform :load "{}"))"#,
                wasm_path.to_str().unwrap(),
                wasm_path.to_str().unwrap()
            );

            let forms = read_many(&script).unwrap();
            assert_eq!(forms.len(), 2, "chess.glia should have 2 forms");

            // First form: (perform host :listen ...) — should succeed.
            let wrapped = wrap_with_handlers(&forms[0]);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert!(
                result.is_ok(),
                "first form failed: {:?}",
                result.unwrap_err()
            );

            // Second form: (perform executor :run ...) — fails because TestExecutor
            // returns "unimplemented" for run_bytes. Expected.
            let wrapped = wrap_with_handlers(&forms[1]);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert!(result.is_err(), "executor run should fail against stub");
        })
        .await;
    }

    // --- run_initd integration ---

    /// run_initd with no WW_ROOT set returns false (no scripts to run).
    #[tokio::test]
    async fn test_run_initd_no_ww_root_skips() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();
            std::env::remove_var("WW_ROOT");
            let blocked = run_initd(&mut env, &ctx, &dispatch).await.unwrap();
            assert!(!blocked, "should not block when WW_ROOT is unset");
        })
        .await;
    }

    /// run_initd with empty WW_ROOT skips gracefully.
    #[tokio::test]
    async fn test_run_initd_empty_ww_root_skips() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();
            std::env::set_var("WW_ROOT", "");
            let blocked = run_initd(&mut env, &ctx, &dispatch).await.unwrap();
            assert!(!blocked, "should not block when WW_ROOT is empty");
        })
        .await;
    }
}
