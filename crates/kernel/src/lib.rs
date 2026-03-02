use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use glia::{read, Val};

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

async fn eval(expr: &Val, ctx: &mut ShellCtx) -> Result<Val, String> {
    match expr {
        Val::List(items) if items.is_empty() => Ok(Val::Nil),
        Val::List(items) => {
            let cmd = match &items[0] {
                Val::Sym(s) => s.as_str(),
                _ => return Err(format!("expected symbol, got {}", items[0])),
            };
            // Resolve session::cap or session.cap compound symbols to the capability name.
            let (resolved_cap, args) = if let Some(cap) = cmd
                .strip_prefix("session::")
                .or_else(|| cmd.strip_prefix("session."))
            {
                (cap, &items[1..])
            } else {
                (cmd, &items[1..])
            };
            match resolved_cap {
                "host" => eval_host(args, ctx).await,
                "executor" => eval_executor(args, ctx).await,
                "ipfs" => eval_ipfs(args, ctx).await,
                "session" => {
                    // (session <cap> <method> [args...]) — session-qualified dispatch
                    let cap = match args.first() {
                        Some(Val::Sym(s)) => s.as_str(),
                        _ => return Err("(session <capability> <method> [args...])".into()),
                    };
                    match cap {
                        "host" => eval_host(&args[1..], ctx).await,
                        "executor" => eval_executor(&args[1..], ctx).await,
                        "ipfs" => eval_ipfs(&args[1..], ctx).await,
                        _ => Err(format!("unknown capability: {cap}")),
                    }
                }
                "cd" => {
                    let path = match args.first() {
                        Some(Val::Str(s)) => s.clone(),
                        Some(Val::Sym(s)) => s.clone(),
                        None => "/".to_string(),
                        _ => return Err("(cd \"<path>\")".into()),
                    };
                    ctx.cwd = path;
                    Ok(Val::Nil)
                }
                "help" => Ok(Val::Str(HELP_TEXT.to_string())),
                "exit" => std::process::exit(0),
                _ => eval_path_lookup(resolved_cap, args, ctx).await,
            }
        }
        // Self-evaluating forms.
        other => Ok(other.clone()),
    }
}

async fn eval_host(args: &[Val], ctx: &ShellCtx) -> Result<Val, String> {
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
        _ => Err(format!("unknown host method: {method}")),
    }
}

async fn eval_executor(args: &[Val], ctx: &ShellCtx) -> Result<Val, String> {
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
        _ => Err(format!("unknown executor method: {method}")),
    }
}

async fn eval_ipfs(args: &[Val], ctx: &ShellCtx) -> Result<Val, String> {
    let method = match args.first() {
        Some(Val::Sym(s)) => s.as_str(),
        _ => return Err("(ipfs <method> [args...])".into()),
    };
    match method {
        "cat" => {
            let path = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(ipfs cat \"<path>\")".into()),
            };
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
            // Try as UTF-8, fall back to hex.
            match std::str::from_utf8(data) {
                Ok(s) => Ok(Val::Str(s.to_string())),
                Err(_) => Ok(Val::Str(format!("<{} bytes>", data.len()))),
            }
        }
        "ls" => {
            let path = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(ipfs ls \"<path>\")".into()),
            };
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

async fn eval_path_lookup(cmd: &str, args: &[Val], ctx: &ShellCtx) -> Result<Val, String> {
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

  (executor echo \"<msg>\")        Diagnostic echo

  (ipfs cat \"<path>\")            Fetch IPFS content
  (ipfs ls \"<path>\")             List IPFS directory

Built-ins:
  (cd \"<path>\")                  Change working directory
  (help)                         This message
  (exit)                         Quit

Unrecognized commands are looked up in PATH (default /bin).";

// ---------------------------------------------------------------------------
// Shell mode (TTY)
// ---------------------------------------------------------------------------

fn write_prompt(stdout: &wasip2::io::streams::OutputStream, cwd: &str) {
    let prompt = format!("{} ❯ ", cwd);
    let _ = stdout.blocking_write_and_flush(prompt.as_bytes());
}

async fn run_shell(mut ctx: ShellCtx) -> Result<(), Box<dyn std::error::Error>> {
    // Boot init.d services before dropping into the REPL.
    // Listeners accept connections in the background (host-side).
    // Initial DHT provide makes us discoverable. The active discovery
    // loop only runs in daemon mode (can't interleave with blocking stdin).
    boot_services(&ctx).await?;

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
                Ok(expr) => match eval(&expr, &mut ctx).await {
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
// Daemon mode (non-TTY) — init.d service bootstrapping
// ---------------------------------------------------------------------------

/// Discovery loop backoff constants.
const BASE_MS: u64 = 2_000;
const MAX_MS: u64 = 900_000;

/// Short peer ID for human-readable logs (last 4 bytes = 8 hex chars).
fn short_id(peer_id: &[u8]) -> String {
    let h = hex::encode(peer_id);
    if h.len() > 8 {
        format!("..{}", &h[h.len() - 8..])
    } else {
        h
    }
}

/// Extract a string value for a keyword key from a glia Map.
fn map_get_str<'a>(pairs: &'a [(Val, Val)], key: &str) -> Option<&'a str> {
    pairs.iter().find_map(|(k, v)| match (k, v) {
        (Val::Keyword(k), Val::Str(s)) if k == key => Some(s.as_str()),
        _ => None,
    })
}

/// A parsed init.d service declaration.
struct ServiceConfig {
    protocol: String,
    handler_wasm: Vec<u8>,
    ns_cid: String,
    seen: Rc<RefCell<HashSet<Vec<u8>>>>,
}

/// Scan `etc/init.d/*.glia` and parse service declarations.
async fn load_initd_services(
    routing: &routing_capnp::routing::Client,
) -> Result<Vec<ServiceConfig>, Box<dyn std::error::Error>> {
    let mut services = Vec::new();

    let dir = match std::fs::read_dir("etc/init.d") {
        Ok(d) => d,
        Err(e) => {
            log::debug!("init.d: no etc/init.d directory ({e}), skipping");
            return Ok(services);
        }
    };

    for entry in dir {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|e| e.to_str()) != Some("glia") {
            continue;
        }

        let content = std::fs::read_to_string(&path)?;
        let config = glia::read(&content)
            .map_err(|e| format!("init.d: parse error in {}: {e}", path.display()))?;

        let pairs = match &config {
            Val::Map(pairs) => pairs,
            _ => {
                log::warn!("init.d: {} is not a map, skipping", path.display());
                continue;
            }
        };

        let protocol = match map_get_str(pairs, "protocol") {
            Some(p) => p.to_string(),
            None => {
                log::warn!("init.d: {} missing :protocol, skipping", path.display());
                continue;
            }
        };

        let handler_path = match map_get_str(pairs, "handler") {
            Some(h) => h.to_string(),
            None => {
                log::warn!("init.d: {} missing :handler, skipping", path.display());
                continue;
            }
        };

        let namespace = match map_get_str(pairs, "namespace") {
            Some(n) => n.to_string(),
            None => {
                log::warn!("init.d: {} missing :namespace, skipping", path.display());
                continue;
            }
        };

        // Load handler WASM from the FHS filesystem.
        let handler_wasm = std::fs::read(&handler_path)
            .map_err(|e| format!("init.d: failed to read {handler_path}: {e}"))?;
        log::debug!(
            "init.d: loaded handler {} ({} bytes)",
            handler_path,
            handler_wasm.len()
        );

        // Hash namespace → deterministic CID for DHT.
        let mut hash_req = routing.hash_request();
        hash_req.get().set_data(namespace.as_bytes());
        let hash_resp = hash_req
            .send()
            .promise
            .await
            .map_err(|e| format!("init.d: hash failed for {namespace}: {e}"))?;
        let ns_cid = hash_resp
            .get()
            .map_err(|e| format!("init.d: hash result error: {e}"))?
            .get_key()
            .map_err(|e| format!("init.d: hash key error: {e}"))?
            .to_str()
            .map_err(|e| format!("init.d: hash key UTF-8 error: {e}"))?
            .to_string();

        services.push(ServiceConfig {
            protocol,
            handler_wasm,
            ns_cid,
            seen: Rc::new(RefCell::new(HashSet::new())),
        });
    }

    Ok(services)
}

// ---------------------------------------------------------------------------
// InitdSink — generic discovery sink that dials peers and spawns handlers
// ---------------------------------------------------------------------------

struct InitdSink {
    dialer: system_capnp::dialer::Client,
    executor: system_capnp::executor::Client,
    handler_wasm: Vec<u8>,
    protocol: String,
    self_id: Vec<u8>,
    seen: Rc<RefCell<HashSet<Vec<u8>>>>,
}

#[allow(refining_impl_trait)]
impl routing_capnp::provider_sink::Server for InitdSink {
    fn provider(
        self: Rc<Self>,
        params: routing_capnp::provider_sink::ProviderParams,
    ) -> Promise<(), capnp::Error> {
        let peer_id = pry!(pry!(pry!(params.get()).get_info()).get_peer_id()).to_vec();

        // Skip self and already-seen peers.
        if peer_id == self.self_id || !self.seen.borrow_mut().insert(peer_id.clone()) {
            return Promise::ok(());
        }

        let dialer = self.dialer.clone();
        let executor = self.executor.clone();
        let handler_wasm = self.handler_wasm.clone();
        let protocol = self.protocol.clone();
        let peer = peer_id.clone();

        Promise::from_future(async move {
            if let Err(e) = dial_and_pump(&dialer, &executor, &handler_wasm, &protocol, &peer).await
            {
                log::error!(
                    "init.d: {}: connection to {} failed: {e}",
                    protocol,
                    short_id(&peer)
                );
            }
            // Brief pause between connections so output is readable.
            let pause = wasip2::clocks::monotonic_clock::subscribe_duration(5_000_000_000); // 5s
            pause.block();
            Ok(())
        })
    }

    fn done(
        self: Rc<Self>,
        _params: routing_capnp::provider_sink::DoneParams,
        _results: routing_capnp::provider_sink::DoneResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// Dial + pump — connect to a discovered peer and wire a handler process
// ---------------------------------------------------------------------------

/// Dial a peer, spawn an initiator handler, and pump data between the
/// dialed stream and the handler's stdin/stdout.
async fn dial_and_pump(
    dialer: &system_capnp::dialer::Client,
    executor: &system_capnp::executor::Client,
    handler_wasm: &[u8],
    protocol: &str,
    peer_id: &[u8],
) -> Result<(), capnp::Error> {
    let them = short_id(peer_id);

    // Dial peer → bidirectional ByteStream.
    let mut dial_req = dialer.dial_request();
    dial_req.get().set_peer(peer_id);
    dial_req.get().set_protocol(protocol);
    let dial_resp = dial_req.send().promise.await?;
    let stream = dial_resp.get()?.get_stream()?;

    log::info!("init.d: {protocol}: dialed {them}");

    // Spawn handler in initiator mode.
    let mut run_req = executor.run_bytes_request();
    run_req.get().set_wasm(handler_wasm);
    {
        let mut env = run_req.get().init_env(4);
        env.set(0, "WW_HANDLER=1");
        env.set(1, "WW_INITIATOR=1");
        env.set(2, format!("WW_PROTOCOL={protocol}"));
        env.set(3, "PATH=/bin");
    }
    let run_resp = run_req.send().promise.await?;
    let process = run_resp.get()?.get_process()?;

    // Get handler stdin/stdout.
    let stdin_resp = process.stdin_request().send().promise.await?;
    let handler_stdin = stdin_resp.get()?.get_stream()?;

    let stdout_resp = process.stdout_request().send().promise.await?;
    let handler_stdout = stdout_resp.get()?.get_stream()?;

    // Pump: handler stdout → dialed stream, dialed stream → handler stdin.
    // Sequential alternating pump — works for turn-based protocols (POC).
    let result = pump_sequential(&stream, &handler_stdin, &handler_stdout).await;

    // Clean up: close handler stdin and the dialed stream.
    let _ = handler_stdin.close_request().send().promise.await;
    let _ = stream.close_request().send().promise.await;

    // Wait for handler process to exit.
    let wait_resp = process.wait_request().send().promise.await?;
    let exit_code = wait_resp.get()?.get_exit_code();
    log::debug!("init.d: {protocol}: handler for {them} exited ({exit_code})");

    result
}

/// Sequential alternating pump between a dialed stream and handler stdio.
///
/// Reads handler stdout → writes to dialed stream, reads dialed stream →
/// writes to handler stdin. This works for turn-based protocols where the
/// initiator writes first. For streaming protocols, a concurrent pump
/// would be needed (future work).
async fn pump_sequential(
    dialed: &system_capnp::byte_stream::Client,
    handler_stdin: &system_capnp::byte_stream::Client,
    handler_stdout: &system_capnp::byte_stream::Client,
) -> Result<(), capnp::Error> {
    loop {
        // Handler wrote a message → read it from stdout.
        let mut read_req = handler_stdout.read_request();
        read_req.get().set_max_bytes(4096);
        let read_resp = read_req.send().promise.await?;
        let data = read_resp.get()?.get_data()?;
        if data.is_empty() {
            break; // Handler closed stdout (game over).
        }

        // Forward to the dialed stream.
        let mut write_req = dialed.write_request();
        write_req.get().set_data(data);
        write_req.send().promise.await?;

        // Read response from the dialed stream.
        let mut read_req = dialed.read_request();
        read_req.get().set_max_bytes(4096);
        let read_resp = read_req.send().promise.await?;
        let data = read_resp.get()?.get_data()?;
        if data.is_empty() {
            break; // Remote closed the stream.
        }

        // Forward response to handler stdin.
        let mut write_req = handler_stdin.write_request();
        write_req.get().set_data(data);
        write_req.send().promise.await?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Init.d boot — shared between shell and daemon modes
// ---------------------------------------------------------------------------

/// Services that were booted and are ready for the discovery loop.
struct BootedServices {
    services: Vec<ServiceConfig>,
    dialer: system_capnp::dialer::Client,
    self_id: Vec<u8>,
}

/// Boot init.d services: load configs, register listeners, initial DHT provide.
///
/// Called at startup in both shell and daemon modes — like an OS, services
/// start regardless of whether you get a login shell.  Returns `None` when
/// no `etc/init.d/*.glia` files exist (kernel-only image).
async fn boot_services(
    ctx: &ShellCtx,
) -> Result<Option<BootedServices>, Box<dyn std::error::Error>> {
    let services = load_initd_services(&ctx.routing)
        .await
        .map_err(|e| e.to_string())?;

    if services.is_empty() {
        log::info!("init.d: no services found");
        return Ok(None);
    }

    log::info!("init.d: loaded {} service(s)", services.len());

    // Get network capabilities.
    let network_resp = ctx
        .host
        .network_request()
        .send()
        .promise
        .await
        .map_err(|e| e.to_string())?;
    let network = network_resp.get().map_err(|e| e.to_string())?;
    let listener = network.get_listener().map_err(|e| e.to_string())?;
    let dialer = network.get_dialer().map_err(|e| e.to_string())?;

    // Register all listeners.
    for svc in &services {
        let mut req = listener.listen_request();
        req.get().set_executor(ctx.executor.clone());
        req.get().set_protocol(&svc.protocol);
        req.get().set_handler(&svc.handler_wasm);
        req.send().promise.await.map_err(|e| e.to_string())?;
        log::info!("init.d: registered /ww/0.1.0/{}", svc.protocol);
    }

    // Resolve peer ID.
    let id_resp = ctx
        .host
        .id_request()
        .send()
        .promise
        .await
        .map_err(|e| e.to_string())?;
    let self_id = id_resp
        .get()
        .map_err(|e| e.to_string())?
        .get_peer_id()
        .map_err(|e| e.to_string())?
        .to_vec();

    // Initial DHT provide so other nodes can find us immediately.
    for svc in &services {
        let mut provide_req = ctx.routing.provide_request();
        provide_req.get().set_key(&svc.ns_cid);
        if let Err(e) = provide_req.send().promise.await {
            log::warn!("init.d: {}: initial provide failed: {e}", svc.protocol);
        }
    }

    log::info!("init.d: peer {}", short_id(&self_id));

    Ok(Some(BootedServices {
        services,
        dialer,
        self_id,
    }))
}

// ---------------------------------------------------------------------------
// Daemon mode — discovery loop
// ---------------------------------------------------------------------------

async fn run_daemon(ctx: ShellCtx) -> Result<(), Box<dyn std::error::Error>> {
    let stderr = get_stderr();

    // Log readiness with peer ID (machine-readable for process managers).
    let id_resp = ctx
        .host
        .id_request()
        .send()
        .promise
        .await
        .map_err(|e| e.to_string())?;
    let peer_id_hex = hex::encode(
        id_resp
            .get()
            .map_err(|e| e.to_string())?
            .get_peer_id()
            .map_err(|e| e.to_string())?,
    );
    let _ = stderr.blocking_write_and_flush(
        format!("{{\"event\":\"ready\",\"peer_id\":\"{peer_id_hex}\"}}\n").as_bytes(),
    );

    // Boot init.d services (listeners + initial provide).
    let booted = boot_services(&ctx).await?;

    let Some(BootedServices {
        services,
        dialer,
        self_id,
    }) = booted
    else {
        // No services — fall back to blocking on stdin (original daemon behavior).
        let stdin = get_stdin();
        loop {
            match stdin.blocking_read(4096) {
                Ok(b) if b.is_empty() => break,
                Err(_) => break,
                Ok(_) => {} // Discard input in daemon mode.
            }
        }
        return Ok(());
    };

    // Discovery loop: provide + find_providers with exponential backoff.
    // Re-provide each pass (DHT records expire). find_providers is a one-shot
    // query, so we loop to catch peers that announce after our search.
    let mut cooldown_ms: u64 = BASE_MS;

    loop {
        let mut found_new = false;

        for svc in &services {
            let prev_seen = svc.seen.borrow().len();

            // Re-provide (DHT records expire).
            let mut provide_req = ctx.routing.provide_request();
            provide_req.get().set_key(&svc.ns_cid);
            if let Err(e) = provide_req.send().promise.await {
                log::warn!("init.d: {}: provide failed: {e}", svc.protocol);
                continue;
            }

            // Search — InitdSink dials new peers automatically.
            let sink: routing_capnp::provider_sink::Client = capnp_rpc::new_client(InitdSink {
                dialer: dialer.clone(),
                executor: ctx.executor.clone(),
                handler_wasm: svc.handler_wasm.clone(),
                protocol: svc.protocol.clone(),
                self_id: self_id.clone(),
                seen: svc.seen.clone(),
            });
            let mut fp_req = ctx.routing.find_providers_request();
            {
                let mut b = fp_req.get();
                b.set_key(&svc.ns_cid);
                b.set_count(5);
                b.set_sink(sink);
            }
            if let Err(e) = fp_req.send().promise.await {
                log::warn!("init.d: {}: findProviders failed: {e}", svc.protocol);
                continue;
            }

            let now_seen = svc.seen.borrow().len();
            if now_seen > prev_seen {
                log::info!("init.d: {}: found {} peer(s)", svc.protocol, now_seen);
                found_new = true;
            }
        }

        // Backoff: reset on new peer, double on idle, cap at MAX.
        if found_new {
            cooldown_ms = BASE_MS;
        } else {
            cooldown_ms = (cooldown_ms * 2).min(MAX_MS);
        }

        // Jitter: uniform in [0.5 * cooldown, cooldown].
        let delay_ms = cooldown_ms / 2 + rand::random_range(0..=cooldown_ms / 2);

        let pause = wasip2::clocks::monotonic_clock::subscribe_duration(
            delay_ms * 1_000_000, // ms → ns
        );
        pause.block();
    }
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

        let ctx = ShellCtx {
            host: results.get_host()?,
            executor: results.get_executor()?,
            ipfs: results.get_ipfs()?,
            routing: results.get_routing()?,
            identity: results.get_identity()?,
            cwd: "/".to_string(),
        };

        let is_tty = std::env::var("WW_TTY").is_ok();
        let result = if is_tty {
            run_shell(ctx).await
        } else {
            run_daemon(ctx).await
        };

        if let Err(e) = result {
            log::error!("kernel error: {e}");
        }

        Ok(())
    });
}

wasip2::cli::command::export!(Kernel);
