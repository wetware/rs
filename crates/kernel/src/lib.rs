use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

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

struct ShellCtx {
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
    &'a mut ShellCtx,
) -> Pin<Box<dyn Future<Output = Result<Val, String>> + 'a>>;

/// Build the dispatch table. Each capability and built-in is registered here.
/// Adding a new verb = one `table.insert(...)` call.
fn build_dispatch() -> HashMap<&'static str, HandlerFn> {
    let mut t: HashMap<&'static str, HandlerFn> = HashMap::new();
    t.insert("host", |a, c| Box::pin(eval_host(a, c)));
    t.insert("executor", |a, c| Box::pin(eval_executor(a, c)));
    t.insert("ipfs", |a, c| Box::pin(eval_ipfs(a, c)));
    t.insert("routing", |a, c| Box::pin(eval_routing(a, c)));
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

fn eval_cd(args: &[Val], ctx: &mut ShellCtx) -> Result<Val, String> {
    let path = match args.first() {
        Some(Val::Str(s)) => s.clone(),
        Some(Val::Sym(s)) => s.clone(),
        None => "/".to_string(),
        _ => return Err("(cd \"<path>\")".into()),
    };
    ctx.cwd = path;
    Ok(Val::Nil)
}

// ---------------------------------------------------------------------------
// Evaluator — resolves expressions via dispatch table + PATH lookup
// ---------------------------------------------------------------------------

fn eval<'a>(
    expr: &'a Val,
    ctx: &'a mut ShellCtx,
    dispatch: &'a HashMap<&'static str, HandlerFn>,
) -> Pin<Box<dyn Future<Output = Result<Val, String>> + 'a>> {
    Box::pin(async move {
        match expr {
            Val::List(items) if items.is_empty() => Ok(Val::Nil),
            Val::List(items) => {
                let cmd = match &items[0] {
                    Val::Sym(s) => s.as_str(),
                    _ => return Err(format!("expected symbol, got {}", items[0])),
                };
                let raw_args = &items[1..];

                // Recursively evaluate any nested list args before dispatching.
                let mut args = Vec::with_capacity(raw_args.len());
                for a in raw_args {
                    match a {
                        Val::List(_) => args.push(eval(a, ctx, dispatch).await?),
                        other => args.push(other.clone()),
                    }
                }

                // Look up in dispatch table, fall through to PATH lookup.
                match dispatch.get(cmd) {
                    Some(handler) => handler(&args, ctx).await,
                    None => eval_path_lookup(cmd, &args, ctx).await,
                }
            }
            // Self-evaluating forms.
            other => Ok(other.clone()),
        }
    }) // Box::pin
}

async fn eval_host(args: &[Val], ctx: &mut ShellCtx) -> Result<Val, String> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err("(host <method> [args...])".into()),
    };
    match method {
        "id" => {
            let resp = ctx
                .host
                .id_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let id = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_peer_id()
                .map_err(|e| e.to_string())?;
            Ok(Val::Str(hex::encode(id)))
        }
        "addrs" => {
            let resp = ctx
                .host
                .addrs_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let addrs = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_addrs()
                .map_err(|e| e.to_string())?;
            let items: Vec<Val> = (0..addrs.len())
                .filter_map(|i| {
                    addrs
                        .get(i)
                        .ok()
                        .and_then(|d| String::from_utf8(d.to_vec()).ok())
                        .map(Val::Str)
                })
                .collect();
            Ok(Val::List(items))
        }
        "peers" => {
            let resp = ctx
                .host
                .peers_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let peers = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_peers()
                .map_err(|e| e.to_string())?;
            let items: Vec<Val> = (0..peers.len())
                .filter_map(|i| {
                    let peer = peers.get(i);
                    let id = peer.get_peer_id().ok().map(hex::encode)?;
                    let addrs = peer.get_addrs().ok()?;
                    let mut entry = vec![Val::Str(id)];
                    for j in 0..addrs.len() {
                        if let Ok(a) = addrs.get(j) {
                            if let Ok(s) = String::from_utf8(a.to_vec()) {
                                entry.push(Val::Str(s));
                            }
                        }
                    }
                    Some(Val::List(entry))
                })
                .collect();
            Ok(Val::List(items))
        }
        "listen" => {
            // (host listen "protocol" <wasm-bytes>)
            let protocol = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(host listen \"<protocol>\" <wasm-bytes>)".into()),
            };
            let wasm = match args.get(2) {
                Some(Val::Bytes(b)) => b.clone(),
                _ => return Err("(host listen \"<protocol>\" <wasm-bytes>): handler must be bytes (use (ipfs cat ...))".into()),
            };

            let network_resp = ctx
                .host
                .network_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let network = network_resp.get().map_err(|e| e.to_string())?;
            let listener = network.get_listener().map_err(|e| e.to_string())?;

            let mut req = listener.listen_request();
            req.get().set_executor(ctx.executor.clone());
            req.get().set_protocol(&protocol);
            req.get().set_handler(&wasm);
            req.send().promise.await.map_err(|e| e.to_string())?;

            log::info!("init.d: registered /ww/0.1.0/{protocol}");
            Ok(Val::Nil)
        }
        _ => Err(format!("unknown host method: {method}")),
    }
}

async fn eval_executor(args: &[Val], ctx: &mut ShellCtx) -> Result<Val, String> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err("(executor <method> [args...])".into()),
    };
    match method {
        "echo" => {
            let msg = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                Some(Val::Sym(s)) => s.clone(),
                _ => return Err("(executor echo \"<message>\")".into()),
            };
            let mut req = ctx.executor.echo_request();
            req.get().set_message(&msg);
            let resp = req.send().promise.await.map_err(|e| e.to_string())?;
            let text = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_response()
                .map_err(|e| e.to_string())?
                .to_str()
                .map_err(|e| e.to_string())?;
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

            let mut req = ctx.executor.run_bytes_request();
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
            let resp = req.send().promise.await.map_err(|e| e.to_string())?;
            let process = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_process()
                .map_err(|e| e.to_string())?;

            // Block until the process exits.
            log::info!("executor run: process spawned, waiting for exit");
            let wait_resp = process
                .wait_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let exit_code = wait_resp.get().map_err(|e| e.to_string())?.get_exit_code();
            log::info!("executor run: process exited ({})", exit_code);
            Ok(Val::Int(exit_code as i64))
        }
        _ => Err(format!("unknown executor method: {method}")),
    }
}

async fn eval_ipfs(args: &[Val], ctx: &mut ShellCtx) -> Result<Val, String> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err("(ipfs <method> [args...])".into()),
    };
    match method {
        "cat" => {
            let raw_path = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(ipfs cat \"<path>\")".into()),
            };
            let path = resolve_ipfs_path(&raw_path);

            let unixfs_resp = ctx
                .ipfs
                .unixfs_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let unixfs = unixfs_resp
                .get()
                .map_err(|e| e.to_string())?
                .get_api()
                .map_err(|e| e.to_string())?;
            let mut req = unixfs.cat_request();
            req.get().set_path(&path);
            let resp = req.send().promise.await.map_err(|e| e.to_string())?;
            let data = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_data()
                .map_err(|e| e.to_string())?;
            Ok(Val::Bytes(data.to_vec()))
        }
        "ls" => {
            let raw_path = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(ipfs ls \"<path>\")".into()),
            };
            let path = resolve_ipfs_path(&raw_path);

            let unixfs_resp = ctx
                .ipfs
                .unixfs_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let unixfs = unixfs_resp
                .get()
                .map_err(|e| e.to_string())?
                .get_api()
                .map_err(|e| e.to_string())?;
            let mut req = unixfs.ls_request();
            req.get().set_path(&path);
            let resp = req.send().promise.await.map_err(|e| e.to_string())?;
            let entries = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_entries()
                .map_err(|e| e.to_string())?;
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
        _ => Err(format!("unknown ipfs method: {method}")),
    }
}

async fn eval_routing(args: &[Val], ctx: &mut ShellCtx) -> Result<Val, String> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err("(routing <method> [args...])".into()),
    };
    match method {
        "provide" => {
            // (routing provide "key")
            let key = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(routing provide \"<key>\")".into()),
            };
            let mut req = ctx.routing.provide_request();
            req.get().set_key(&key);
            req.send().promise.await.map_err(|e| e.to_string())?;
            Ok(Val::Nil)
        }
        "hash" => {
            // (routing hash "data")
            let data = match args.get(1) {
                Some(Val::Str(s)) => s.as_bytes().to_vec(),
                Some(Val::Bytes(b)) => b.clone(),
                _ => return Err("(routing hash \"<data>\")".into()),
            };
            let mut req = ctx.routing.hash_request();
            req.get().set_data(&data);
            let resp = req.send().promise.await.map_err(|e| e.to_string())?;
            let key = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_key()
                .map_err(|e| e.to_string())?
                .to_str()
                .map_err(|e| e.to_string())?;
            Ok(Val::Str(key.to_string()))
        }
        _ => Err(format!("unknown routing method: {method}")),
    }
}

async fn eval_path_lookup(cmd: &str, args: &[Val], ctx: &mut ShellCtx) -> Result<Val, String> {
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
            let mut req = ctx.executor.run_bytes_request();
            {
                let mut b = req.get();
                b.set_wasm(&bytes);
                let mut arg_list = b.init_args(str_args.len() as u32);
                for (i, a) in str_args.iter().enumerate() {
                    arg_list.set(i as u32, a);
                }
            }
            let resp = req.send().promise.await.map_err(|e| e.to_string())?;
            let process = resp
                .get()
                .map_err(|e| e.to_string())?
                .get_process()
                .map_err(|e| e.to_string())?;

            // Read stdout to completion.
            let stdout_resp = process
                .stdout_request()
                .send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            let stdout_stream = stdout_resp
                .get()
                .map_err(|e| e.to_string())?
                .get_stream()
                .map_err(|e| e.to_string())?;

            let mut output = Vec::new();
            loop {
                let mut req = stdout_stream.read_request();
                req.get().set_max_bytes(65536);
                let resp = req.send().promise.await.map_err(|e| e.to_string())?;
                let chunk = resp
                    .get()
                    .map_err(|e| e.to_string())?
                    .get_data()
                    .map_err(|e| e.to_string())?;
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
                .map_err(|e| e.to_string())?;
            let exit_code = wait_resp.get().map_err(|e| e.to_string())?.get_exit_code();

            let out_str = String::from_utf8_lossy(&output).trim_end().to_string();
            if exit_code != 0 {
                return Err(format!("{cmd}: exit code {exit_code}\n{out_str}"));
            }
            return Ok(Val::Str(out_str));
        }
    }
    Err(format!("{cmd}: command not found"))
}

const HELP_TEXT: &str = "\
Capabilities:
  (host id)                      Peer ID
  (host addrs)                   Listen addresses
  (host peers)                   Connected peers
  (host listen \"p\" <wasm>)       Register protocol handler

  (executor echo \"<msg>\")        Diagnostic echo
  (executor run <wasm> :env {})  Spawn foreground process

  (ipfs cat \"<path>\")            Fetch IPFS content (bytes)
  (ipfs ls \"<path>\")             List IPFS directory

  (routing provide \"<key>\")      Announce to DHT
  (routing hash \"<data>\")        Hash data to CID

Built-ins:
  (cd \"<path>\")                  Change working directory
  (help)                         This message
  (exit)                         Quit

Unrecognized commands are looked up in PATH (default /bin).";

// ---------------------------------------------------------------------------
// Init.d — evaluate scripts from $WW_ROOT/etc/init.d/*.glia
// ---------------------------------------------------------------------------

/// Scan `$WW_ROOT/etc/init.d/*.glia` via IPFS UnixFS, parse and evaluate
/// each file as a glia script. Returns true if any expression blocked
/// (i.e. a foreground process ran to completion via `(executor run ...)`).
async fn run_initd(
    ctx: &mut ShellCtx,
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
        .ipfs
        .unixfs_request()
        .send()
        .promise
        .await
        .map_err(|e| e.to_string())?;
    let unixfs = unixfs_resp
        .get()
        .map_err(|e| e.to_string())?
        .get_api()
        .map_err(|e| e.to_string())?;

    // ls $WW_ROOT/etc/init.d — gracefully return empty on error
    // (no /etc or no /etc/init.d = nothing configured).
    let initd_path = format!("{root}/etc/init.d");
    let mut ls_req = unixfs.ls_request();
    ls_req.get().set_path(&initd_path);
    let entries = match ls_req.send().promise.await {
        Ok(resp) => {
            let reader = resp.get().map_err(|e| e.to_string())?;
            let list = reader.get_entries().map_err(|e| e.to_string())?;
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

        let content = match std::str::from_utf8(&data) {
            Ok(s) => s,
            Err(e) => {
                log::error!("init.d: {name}: not valid UTF-8: {e}");
                continue;
            }
        };

        let forms = match read_many(content) {
            Ok(f) => f,
            Err(e) => {
                log::error!("init.d: {name}: parse error: {e}");
                continue;
            }
        };

        log::info!("init.d: evaluating {name} ({} form(s))", forms.len());

        for (i, form) in forms.iter().enumerate() {
            log::info!("init.d: {name}: evaluating form {}/{}", i + 1, forms.len());
            match eval(form, ctx, dispatch).await {
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
    mut ctx: ShellCtx,
    dispatch: &HashMap<&'static str, HandlerFn>,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = get_stdin();
    let stdout = get_stdout();
    let stderr = get_stderr();

    write_prompt(&stdout, &ctx.cwd);
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
                    write_prompt(&stdout, &ctx.cwd);
                    continue;
                }
            };

            if line.is_empty() {
                write_prompt(&stdout, &ctx.cwd);
                continue;
            }

            match read(line) {
                Ok(expr) => match eval(&expr, &mut ctx, dispatch).await {
                    Ok(Val::Nil) => {}
                    Ok(result) => {
                        let _ = stdout.blocking_write_and_flush(format!("{result}\n").as_bytes());
                    }
                    Err(e) => {
                        let _ = stderr.blocking_write_and_flush(format!("error: {e}\n").as_bytes());
                    }
                },
                Err(e) => {
                    let _ =
                        stderr.blocking_write_and_flush(format!("parse error: {e}\n").as_bytes());
                }
            }

            write_prompt(&stdout, &ctx.cwd);
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

        let mut ctx = ShellCtx {
            host: results.get_host()?,
            executor: results.get_executor()?,
            ipfs: results.get_ipfs()?,
            routing: results.get_routing()?,
            identity: results.get_identity()?,
            cwd: "/".to_string(),
        };

        let dispatch = build_dispatch();

        // Run init.d scripts first. If a foreground process blocked
        // (e.g. `(executor run ...)` in the script), we're done.
        let blocked = run_initd(&mut ctx, &dispatch).await.unwrap_or_else(|e| {
            log::error!("init.d: {e}");
            false
        });

        if !blocked {
            let is_tty = std::env::var("WW_TTY").is_ok();
            let result = if is_tty {
                run_shell(ctx, &dispatch).await
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
}
