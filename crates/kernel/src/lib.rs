use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use glia::eval::{self, Dispatch, Env};
use glia::{read, read_many, Val};

use wasip2::cli::stderr::get_stderr;
use wasip2::cli::stdin::get_stdin;
use wasip2::cli::stdout::get_stdout;
use wasip2::exports::cli::run::Guest;

#[allow(dead_code)]
mod system_capnp {
    include!(concat!(env!("OUT_DIR"), "/system_capnp.rs"));
}

#[allow(dead_code)]
mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/stem_capnp.rs"));
}

#[allow(dead_code)]
mod ipfs_capnp {
    include!(concat!(env!("OUT_DIR"), "/ipfs_capnp.rs"));
}

#[allow(dead_code)]
mod routing_capnp {
    include!(concat!(env!("OUT_DIR"), "/routing_capnp.rs"));
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
// Evaluator — dispatches (capability method args...) to RPC calls
// ---------------------------------------------------------------------------

struct Session {
    host: system_capnp::host::Client,
    executor: system_capnp::executor::Client,
    ipfs: ipfs_capnp::client::Client,
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
// Dispatch table — single source of truth for command routing
// ---------------------------------------------------------------------------

/// Async handler: takes evaluated args and the shell context.
type HandlerFn = for<'a> fn(
    &'a [Val],
    &'a RefCell<Session>,
) -> Pin<Box<dyn Future<Output = Result<Val, Val>> + 'a>>;

/// Build the dispatch table. Each capability and built-in is registered here.
/// Adding a new verb = one `table.insert(...)` call.
fn build_dispatch() -> HashMap<&'static str, HandlerFn> {
    let mut t: HashMap<&'static str, HandlerFn> = HashMap::new();
    t.insert("host", |a, c| Box::pin(eval_host(a, c)));
    t.insert("executor", |a, c| Box::pin(eval_executor(a, c)));
    t.insert("ipfs", |a, c| Box::pin(eval_ipfs(a, c)));
    t.insert("routing", |a, c| Box::pin(eval_routing(a, c)));
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

/// (load "path") — read bytes from the virtual filesystem.
/// Works for local paths and /ipfs/ paths (via WASI interceptor).
fn eval_load(args: &[Val]) -> Result<Val, Val> {
    let path = match args.first() {
        Some(Val::Str(s)) => s.clone(),
        _ => return Err("(load \"<path>\")".into()),
    };
    let resolved = resolve_ipfs_path(&path);
    std::fs::read(&resolved)
        .map(Val::Bytes)
        .map_err(|e| Val::from(format!("load: {resolved}: {e}")))
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

async fn eval_host(args: &[Val], ctx: &RefCell<Session>) -> Result<Val, Val> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err(Val::from("(host <method> [args...])")),
    };
    match method {
        "id" => {
            let resp = ctx
                .borrow()
                .host
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
            Ok(Val::Str(bs58::encode(id).into_string()))
        }
        "addrs" => {
            let resp = ctx
                .borrow()
                .host
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
            Ok(Val::List(items))
        }
        "peers" => {
            let resp = ctx
                .borrow()
                .host
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
                                .and_then(|a| multiaddr::Multiaddr::try_from(a.to_vec()).ok())
                                .map(|m| Val::Str(m.to_string()))
                        })
                        .collect();
                    Some(Val::Map(vec![
                        (Val::Keyword("peer-id".into()), Val::Str(id)),
                        (Val::Keyword("addrs".into()), Val::List(addr_vals)),
                    ]))
                })
                .collect();
            Ok(Val::List(items))
        }
        "listen" => {
            // Unified listen: arity determines mode.
            //   (host listen <wasm>)              → VatListener (schema in WASM)
            //   (host listen "protocol" <wasm>)   → StreamListener
            match args.len() {
                2 => {
                    // 1 arg: VatListener mode — WASM has schema.capnp custom section.
                    let wasm = match args.get(1) {
                        Some(Val::Bytes(b)) => b.clone(),
                        _ => return Err("(host listen <wasm>): expected bytes".into()),
                    };
                    let network_resp = ctx
                        .borrow()
                        .host
                        .network_request()
                        .send()
                        .promise
                        .await
                        .map_err(|e| Val::from(e.to_string()))?;
                    let network = network_resp.get().map_err(|e| Val::from(e.to_string()))?;
                    let listener = network
                        .get_vat_listener()
                        .map_err(|e| Val::from(e.to_string()))?;
                    let mut req = listener.listen_request();
                    req.get().set_executor(ctx.borrow().executor.clone());
                    req.get().set_wasm(&wasm);
                    req.send()
                        .promise
                        .await
                        .map_err(|e| Val::from(e.to_string()))?;
                    log::info!("init.d: registered vat handler");
                    Ok(Val::Nil)
                }
                3 => {
                    // 2 args: StreamListener mode — explicit protocol name.
                    let protocol =
                        match args.get(1) {
                            Some(Val::Str(s)) => s.clone(),
                            _ => return Err(
                                "(host listen \"<protocol>\" <wasm>): first arg must be a string"
                                    .into(),
                            ),
                        };
                    let wasm = match args.get(2) {
                        Some(Val::Bytes(b)) => b.clone(),
                        _ => {
                            return Err(
                                "(host listen \"<protocol>\" <wasm>): second arg must be bytes"
                                    .into(),
                            )
                        }
                    };
                    let network_resp = ctx
                        .borrow()
                        .host
                        .network_request()
                        .send()
                        .promise
                        .await
                        .map_err(|e| Val::from(e.to_string()))?;
                    let network = network_resp.get().map_err(|e| Val::from(e.to_string()))?;
                    let listener = network
                        .get_stream_listener()
                        .map_err(|e| Val::from(e.to_string()))?;
                    let mut req = listener.listen_request();
                    req.get().set_executor(ctx.borrow().executor.clone());
                    req.get().set_protocol(&protocol);
                    req.get().set_wasm(&wasm);
                    req.send()
                        .promise
                        .await
                        .map_err(|e| Val::from(e.to_string()))?;
                    log::info!("init.d: registered stream handler /ww/0.1.0/stream/{protocol}");
                    Ok(Val::Nil)
                }
                _ => Err("(host listen <wasm>) or (host listen \"protocol\" <wasm>)".into()),
            }
        }
        _ => Err(Val::from(format!("unknown host method: {method}"))),
    }
}

async fn eval_executor(args: &[Val], ctx: &RefCell<Session>) -> Result<Val, Val> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err(Val::from("(executor <method> [args...])")),
    };
    match method {
        "echo" => {
            let msg = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                Some(Val::Sym(s)) => s.clone(),
                _ => return Err("(executor echo \"<message>\")".into()),
            };
            let mut req = ctx.borrow().executor.echo_request();
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
            Ok(Val::Str(text.to_string()))
        }
        "run" => {
            // (executor run <wasm-bytes> :env {"KEY" "VAL" ...})
            let wasm = match args.get(1) {
                Some(Val::Bytes(b)) => b.clone(),
                _ => {
                    return Err(
                        "(executor run <wasm-bytes> [:env {map}]): wasm must be bytes".into(),
                    )
                }
            };

            // Parse optional keyword args: :env {map}
            let mut env_pairs: Vec<String> = Vec::new();
            let mut i = 2;
            while i < args.len() {
                match &args[i] {
                    Val::Keyword(k) if k == "env" => {
                        i += 1;
                        if let Some(Val::Map(pairs)) = args.get(i) {
                            for (k, v) in pairs {
                                let key = match k {
                                    Val::Str(s) => s.clone(),
                                    Val::Sym(s) => s.clone(),
                                    other => format!("{other}"),
                                };
                                let val = match v {
                                    Val::Str(s) => s.clone(),
                                    Val::Sym(s) => s.clone(),
                                    other => format!("{other}"),
                                };
                                env_pairs.push(format!("{key}={val}"));
                            }
                        }
                    }
                    _ => {}
                }
                i += 1;
            }

            log::info!(
                "executor run: spawning process ({} bytes, {} env vars)",
                wasm.len(),
                env_pairs.len()
            );

            let mut req = ctx.borrow().executor.run_bytes_request();
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

            // Block until the process exits.
            log::info!("executor run: process spawned, waiting for exit");
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
            log::info!("executor run: process exited ({})", exit_code);
            Ok(Val::Int(exit_code as i64))
        }
        _ => Err(Val::from(format!("unknown executor method: {method}"))),
    }
}

async fn eval_ipfs(args: &[Val], ctx: &RefCell<Session>) -> Result<Val, Val> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err(Val::from("(ipfs <method> [args...])")),
    };
    match method {
        "cat" => {
            let raw_path = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(ipfs cat \"<path>\")".into()),
            };
            let path = resolve_ipfs_path(&raw_path);

            let unixfs_resp = ctx
                .borrow()
                .ipfs
                .unixfs_request()
                .send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            let unixfs = unixfs_resp
                .get()
                .map_err(|e| Val::from(e.to_string()))?
                .get_api()
                .map_err(|e| Val::from(e.to_string()))?;
            let mut req = unixfs.cat_request();
            req.get().set_path(&path);
            let resp = req
                .send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            let data = resp
                .get()
                .map_err(|e| Val::from(e.to_string()))?
                .get_data()
                .map_err(|e| Val::from(e.to_string()))?;
            Ok(Val::Bytes(data.to_vec()))
        }
        "ls" => {
            let raw_path = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(ipfs ls \"<path>\")".into()),
            };
            let path = resolve_ipfs_path(&raw_path);

            let unixfs_resp = ctx
                .borrow()
                .ipfs
                .unixfs_request()
                .send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            let unixfs = unixfs_resp
                .get()
                .map_err(|e| Val::from(e.to_string()))?
                .get_api()
                .map_err(|e| Val::from(e.to_string()))?;
            let mut req = unixfs.ls_request();
            req.get().set_path(&path);
            let resp = req
                .send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            let entries = resp
                .get()
                .map_err(|e| Val::from(e.to_string()))?
                .get_entries()
                .map_err(|e| Val::from(e.to_string()))?;
            let items: Vec<Val> = (0..entries.len())
                .filter_map(|i| {
                    let e = entries.get(i);
                    let name = e.get_name().ok()?.to_str().ok()?;
                    let size = e.get_size();
                    Some(Val::List(vec![
                        Val::Str(name.to_string()),
                        Val::Sym(size.to_string()),
                    ]))
                })
                .collect();
            Ok(Val::List(items))
        }
        _ => Err(Val::from(format!("unknown ipfs method: {method}"))),
    }
}

/// Hash a name to a CID via the host's routing.hash() RPC.
async fn routing_hash(
    routing: &routing_capnp::routing::Client,
    name: &str,
) -> Result<String, String> {
    let mut req = routing.hash_request();
    req.get().set_data(name.as_bytes());
    let resp = req.send().promise.await.map_err(|e| e.to_string())?;
    resp.get()
        .map_err(|e| e.to_string())?
        .get_key()
        .map_err(|e| e.to_string())?
        .to_str()
        .map(|s| s.to_string())
        .map_err(|e| e.to_string())
}

/// ProviderSink that collects streamed results into a channel.
/// The guest is single-threaded WASM — the capnp-rpc event loop
/// dispatches all provider() and done() callbacks before findProviders
/// resolves, so try_recv() on the consumer side drains the full result set.
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

async fn eval_routing(args: &[Val], ctx: &RefCell<Session>) -> Result<Val, Val> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err(Val::from("(routing <method> [args...])")),
    };
    match method {
        "provide" => {
            // (routing provide "name") — hashes internally, then announces to DHT.
            let name = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(routing provide \"<name>\")".into()),
            };
            let cid = routing_hash(&ctx.borrow().routing, &name).await?;
            let mut req = ctx.borrow().routing.provide_request();
            req.get().set_key(&cid);
            req.send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;
            Ok(Val::Nil)
        }
        "find" => {
            // (routing find "name")            — default count 20
            // (routing find "name" :count 5)   — override count
            let name = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(routing find \"<name>\" [:count N])".into()),
            };
            // Parse optional :count keyword.
            // Positive value = limit; zero or negative = no limit (u32::MAX).
            let mut count: u32 = 20;
            let mut i = 2;
            while i < args.len() {
                if let Val::Keyword(k) = &args[i] {
                    if k == "count" {
                        i += 1;
                        if let Some(Val::Int(n)) = args.get(i) {
                            count = if *n <= 0 { u32::MAX } else { *n as u32 };
                        }
                    }
                }
                i += 1;
            }

            let cid = routing_hash(&ctx.borrow().routing, &name).await?;

            // Create a CollectorSink to receive streamed providers.
            let (tx, rx) = std::sync::mpsc::channel();
            let sink: routing_capnp::provider_sink::Client =
                capnp_rpc::new_client(CollectorSink { tx });

            let mut req = ctx.borrow().routing.find_providers_request();
            req.get().set_key(&cid);
            req.get().set_count(count);
            req.get().set_sink(sink);
            req.send()
                .promise
                .await
                .map_err(|e| Val::from(e.to_string()))?;

            // Collect results from the channel. Single-threaded capnp-rpc
            // guarantees all provider() callbacks complete before findProviders
            // resolves, so try_recv() drains the full result set.
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
            Ok(Val::List(providers))
        }
        "hash" => {
            // (routing hash "data") — exposed for advanced use; provide hashes internally.
            let data = match args.get(1) {
                Some(Val::Str(s)) => s.as_bytes().to_vec(),
                Some(Val::Bytes(b)) => b.clone(),
                _ => return Err("(routing hash \"<data>\")".into()),
            };
            let mut req = ctx.borrow().routing.hash_request();
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
            Ok(Val::Str(key.to_string()))
        }
        _ => Err(Val::from(format!("unknown routing method: {method}"))),
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
Capabilities:
  (host id)                      Peer ID
  (host addrs)                   Listen addresses
  (host peers)                   Connected peers
  (host listen <wasm>)           Register RPC handler (schema in WASM)
  (host listen \"p\" <wasm>)       Register stream handler

  (executor echo \"<msg>\")        Diagnostic echo
  (executor run <wasm> :env {})  Spawn foreground process

  (ipfs cat \"<path>\")            Fetch IPFS content (bytes)
  (ipfs ls \"<path>\")             List IPFS directory

  (routing provide \"<name>\")      Announce to DHT (hashes internally)
  (routing find \"<name>\" [:count N])  Discover providers (default 20)
  (routing hash \"<data>\")        Hash data to CID

Effects:
  (perform :load \"<path>\")       Load bytes from virtual filesystem

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

/// Wrap a form in `(with-effect-handler {:load (fn [path resume] (resume (load path)))} <form>)`.
/// This installs default effect handlers so init.d scripts (and the shell)
/// can use `(perform :load "path")` to load bytes from the virtual filesystem.
fn wrap_with_default_effects(form: &Val) -> Val {
    // (with-effect-handler {:load (fn [path resume] (resume (load path)))} <form>)
    Val::List(vec![
        Val::Sym("with-effect-handler".into()),
        Val::Map(vec![(
            Val::Keyword("load".into()),
            Val::List(vec![
                Val::Sym("fn".into()),
                Val::Vector(vec![Val::Sym("path".into()), Val::Sym("resume".into())]),
                Val::List(vec![
                    Val::Sym("resume".into()),
                    Val::List(vec![Val::Sym("load".into()), Val::Sym("path".into())]),
                ]),
            ]),
        )]),
        form.clone(),
    ])
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

    // Get the UnixFS API.
    let unixfs_resp = ctx
        .borrow()
        .ipfs
        .unixfs_request()
        .send()
        .promise
        .await
        .map_err(|e| Val::from(e.to_string()))?;
    let unixfs = unixfs_resp
        .get()
        .map_err(|e| Val::from(e.to_string()))?
        .get_api()
        .map_err(|e| Val::from(e.to_string()))?;

    // ls $WW_ROOT/etc/init.d — gracefully return empty on error
    // (no /etc or no /etc/init.d = nothing configured).
    let initd_path = format!("{root}/etc/init.d");
    let mut ls_req = unixfs.ls_request();
    ls_req.get().set_path(&initd_path);
    let entries = match ls_req.send().promise.await {
        Ok(resp) => {
            let reader = resp.get().map_err(|e| Val::from(e.to_string()))?;
            let list = reader.get_entries().map_err(|e| Val::from(e.to_string()))?;
            let mut names = Vec::new();
            for i in 0..list.len() {
                let entry = list.get(i);
                if let Ok(name) = entry.get_name() {
                    if let Ok(s) = name.to_str() {
                        if s.ends_with(".glia") {
                            names.push(s.to_string());
                        }
                    }
                }
            }
            names
        }
        Err(e) => {
            log::debug!("init.d: ls {initd_path} failed ({e}), skipping");
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

        // cat the glia script — failure skips this script.
        let mut cat_req = unixfs.cat_request();
        cat_req.get().set_path(&script_path);
        let data = match cat_req.send().promise.await {
            Ok(resp) => match resp.get().and_then(|r| r.get_data()) {
                Ok(d) => d.to_vec(),
                Err(e) => {
                    log::error!("init.d: {name}: failed to read response: {e}");
                    continue;
                }
            },
            Err(e) => {
                log::error!("init.d: {name}: cat failed: {e}");
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
            let wrapped = wrap_with_default_effects(form);
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
                    let wrapped = wrap_with_default_effects(&expr);
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
            ipfs: results.get_ipfs()?,
            routing: results.get_routing()?,
            identity: results.get_identity()?,
            cwd: "/".to_string(),
        });

        let dispatch = build_dispatch();
        let mut env = Env::new();

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

    // --- wrap_with_default_effects ---

    #[test]
    fn wrap_with_default_effects_produces_with_effect_handler() {
        let form = Val::List(vec![Val::Sym("host".into()), Val::Sym("id".into())]);
        let wrapped = wrap_with_default_effects(&form);
        // Should be (with-effect-handler {:load <fn>} <form>)
        if let Val::List(items) = &wrapped {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0], Val::Sym("with-effect-handler".into()));
            if let Val::Map(pairs) = &items[1] {
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].0, Val::Keyword("load".into()));
            } else {
                panic!("expected Map, got {:?}", items[1]);
            }
            assert_eq!(items[2], form);
        } else {
            panic!("expected List");
        }
    }

    // --- dispatch table ---

    #[test]
    fn dispatch_table_has_all_verbs() {
        let table = build_dispatch();
        let expected = [
            "host", "executor", "ipfs", "routing", "load", "cd", "help", "exit",
        ];
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
    /// capnp-rpc clients are !Send, so we need LocalSet::run_until.
    async fn run_local<F, T>(f: F) -> T
    where
        F: Future<Output = T>,
    {
        tokio::task::LocalSet::new().run_until(f).await
    }

    // --- host tests ---

    #[tokio::test]
    async fn test_host_id_returns_bs58() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("id".into())];
            let result = eval_host(&args, &ctx).await.unwrap();
            let expected = bs58::encode(STUB_PEER_ID).into_string();
            assert_eq!(result, Val::Str(expected));
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_addrs_returns_multiaddr_strings() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("addrs".into())];
            let result = eval_host(&args, &ctx).await.unwrap();
            match result {
                Val::List(addrs) => {
                    assert_eq!(addrs.len(), 1);
                    // STUB_MULTIADDR = /ip4/127.0.0.1/tcp/4001
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
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("peers".into())];
            let result = eval_host(&args, &ctx).await.unwrap();
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
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("bogus".into())];
            let err = eval_host(&args, &ctx).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("unknown host method"), "got: {msg}");
        })
        .await;
    }

    // --- host listen tests ---

    #[tokio::test]
    async fn test_host_listen_vat_passes_executor() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            // (host listen <wasm-bytes>) — VatListener mode
            let args = vec![Val::Sym("listen".into()), Val::Bytes(b"fake-wasm".to_vec())];
            let result = eval_host(&args, &ctx).await;
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
            let ctx = RefCell::new(test_session());
            // (host listen "my-protocol" <wasm-bytes>) — StreamListener mode
            let args = vec![
                Val::Sym("listen".into()),
                Val::Str("my-protocol".into()),
                Val::Bytes(b"fake-wasm".to_vec()),
            ];
            let result = eval_host(&args, &ctx).await;
            assert!(
                result.is_ok(),
                "StreamListener listen failed: {:?}",
                result.unwrap_err()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_host_listen_wrong_arity_returns_error() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            // 0 args after "listen" — should error
            let args = vec![Val::Sym("listen".into())];
            assert!(eval_host(&args, &ctx).await.is_err());
            // 3 args after "listen" — should error
            let args = vec![
                Val::Sym("listen".into()),
                Val::Str("a".into()),
                Val::Bytes(b"b".to_vec()),
                Val::Str("extra".into()),
            ];
            assert!(eval_host(&args, &ctx).await.is_err());
        })
        .await;
    }

    // --- executor tests ---

    #[tokio::test]
    async fn test_executor_echo() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("echo".into()), Val::Str("hello".into())];
            let result = eval_executor(&args, &ctx).await.unwrap();
            assert_eq!(result, Val::Str("hello".into()));
        })
        .await;
    }

    // --- routing tests ---

    #[tokio::test]
    async fn test_routing_provide_succeeds() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("provide".into()), Val::Str("oracle".into())];
            let result = eval_routing(&args, &ctx).await.unwrap();
            assert_eq!(result, Val::Nil);
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_provide_missing_name() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("provide".into())];
            let err = eval_routing(&args, &ctx).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("routing provide"), "got: {msg}");
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_find_default_count() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("find".into()), Val::Str("oracle".into())];
            let result = eval_routing(&args, &ctx).await.unwrap();
            match result {
                Val::List(providers) => {
                    // TestRouting streams min(count, 2) = min(20, 2) = 2 providers
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
            let ctx = RefCell::new(test_session());
            let args = vec![
                Val::Sym("find".into()),
                Val::Str("oracle".into()),
                Val::Keyword("count".into()),
                Val::Int(1),
            ];
            let result = eval_routing(&args, &ctx).await.unwrap();
            match result {
                Val::List(providers) => {
                    // TestRouting streams min(1, 2) = 1 provider
                    assert_eq!(providers.len(), 1);
                }
                other => panic!("expected list, got {other:?}"),
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_find_zero_count_means_no_limit() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![
                Val::Sym("find".into()),
                Val::Str("oracle".into()),
                Val::Keyword("count".into()),
                Val::Int(0),
            ];
            let result = eval_routing(&args, &ctx).await.unwrap();
            match result {
                Val::List(providers) => {
                    // count=0 → u32::MAX, TestRouting streams min(u32::MAX, 2) = 2
                    assert_eq!(providers.len(), 2);
                }
                other => panic!("expected list, got {other:?}"),
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_find_missing_name() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("find".into())];
            let err = eval_routing(&args, &ctx).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("routing find"), "got: {msg}");
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_hash() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("hash".into()), Val::Str("test-data".into())];
            let result = eval_routing(&args, &ctx).await.unwrap();
            assert_eq!(result, Val::Str("QmTestCid123".into()));
        })
        .await;
    }

    #[tokio::test]
    async fn test_routing_unknown_method_returns_error() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let args = vec![Val::Sym("bogus".into())];
            let err = eval_routing(&args, &ctx).await.unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("unknown routing method"), "got: {msg}");
        })
        .await;
    }

    // --- perform :load effect round-trip ---

    /// Verify that (perform :load "path") inside wrap_with_default_effects
    /// actually resolves through the effect handler → eval_load → filesystem.
    #[tokio::test]
    async fn test_perform_load_resolves_through_effect_handler() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();

            // Write a temp file so eval_load can read it.
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("test.bin");
            std::fs::write(&file_path, b"hello-bytes").unwrap();

            // Clear WW_ROOT so resolve_ipfs_path doesn't mangle the absolute path.
            std::env::remove_var("WW_ROOT");

            // (perform :load "/absolute/path/test.bin")
            let form = Val::List(vec![
                Val::Sym("perform".into()),
                Val::Keyword("load".into()),
                Val::Str(file_path.to_str().unwrap().to_string()),
            ]);
            let wrapped = wrap_with_default_effects(&form);
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

            std::env::remove_var("WW_ROOT");

            let form = Val::List(vec![
                Val::Sym("perform".into()),
                Val::Keyword("load".into()),
                Val::Str("/nonexistent/path/missing.wasm".to_string()),
            ]);
            let wrapped = wrap_with_default_effects(&form);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert!(result.is_err(), "expected error for missing file");
        })
        .await;
    }

    // --- init script eval integration ---

    /// Eval (host listen (perform :load "path")) end-to-end: the effect handler
    /// resolves :load to file bytes, then the kernel dispatches (host listen <bytes>)
    /// to the VatListener stub.
    #[tokio::test]
    async fn test_chess_glia_listen_form_evals_end_to_end() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();

            // Write fake WASM so (perform :load ...) has something to read.
            let dir = tempfile::tempdir().unwrap();
            let wasm_path = dir.path().join("chess-demo.wasm");
            std::fs::write(&wasm_path, b"fake-wasm-bytes").unwrap();

            // Clear WW_ROOT so resolve_ipfs_path doesn't mangle the path.
            std::env::remove_var("WW_ROOT");

            // Parse the actual chess.glia form syntax:
            // (host listen (perform :load "/path/to/chess-demo.wasm"))
            let script = format!(
                r#"(host listen (perform :load "{}"))"#,
                wasm_path.to_str().unwrap()
            );
            let form = read(&script).unwrap();
            let wrapped = wrap_with_default_effects(&form);
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
    /// Both forms should succeed: (host listen ...) and (executor run ...).
    /// executor.run_bytes is stubbed to return an error (no real WASM runtime),
    /// so we only test that the first form (host listen) evaluates cleanly.
    #[tokio::test]
    async fn test_chess_glia_full_script_parses_and_first_form_evals() {
        run_local(async {
            let ctx = RefCell::new(test_session());
            let dispatch = build_dispatch();
            let mut env = Env::new();

            // Write fake WASM.
            let dir = tempfile::tempdir().unwrap();
            let wasm_path = dir.path().join("chess-demo.wasm");
            std::fs::write(&wasm_path, b"fake-wasm-bytes").unwrap();

            // Clear WW_ROOT so resolve_ipfs_path doesn't mangle the path.
            std::env::remove_var("WW_ROOT");

            // Simulate chess.glia with absolute paths (since we don't have
            // the IPFS virtual filesystem in test mode).
            let script = format!(
                r#"(host listen (perform :load "{}"))
                   (executor run (perform :load "{}"))"#,
                wasm_path.to_str().unwrap(),
                wasm_path.to_str().unwrap()
            );

            let forms = read_many(&script).unwrap();
            assert_eq!(forms.len(), 2, "chess.glia should have 2 forms");

            // First form: (host listen ...) — should succeed via VatListener stub.
            let wrapped = wrap_with_default_effects(&forms[0]);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert!(
                result.is_ok(),
                "first form (host listen) failed: {:?}",
                result.unwrap_err()
            );

            // Second form: (executor run ...) — will fail because TestExecutor
            // returns "unimplemented" for run_bytes. That's expected.
            let wrapped = wrap_with_default_effects(&forms[1]);
            let result = eval(&wrapped, &mut env, &ctx, &dispatch).await;
            assert!(
                result.is_err(),
                "executor run should fail against stub (no real WASM runtime)"
            );
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
