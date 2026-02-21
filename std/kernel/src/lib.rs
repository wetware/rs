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
// S-expression reader/printer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Val {
    Sym(String),
    Str(String),
    List(Vec<Val>),
    Nil,
}

impl core::fmt::Display for Val {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Val::Sym(s) => write!(f, "{s}"),
            Val::Str(s) => write!(f, "\"{s}\""),
            Val::List(items) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, ")")
            }
            Val::Nil => write!(f, "nil"),
        }
    }
}

fn read(input: &str) -> Result<Val, String> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Err("empty input".into());
    }
    let (val, rest) = parse_tokens(&tokens)?;
    if !rest.is_empty() {
        return Err("unexpected tokens after expression".into());
    }
    Ok(val)
}

fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                chars.next();
            }
            '(' | ')' => {
                tokens.push(c.to_string());
                chars.next();
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('\\') => match chars.next() {
                            Some(esc) => s.push(esc),
                            None => return Err("unterminated string escape".into()),
                        },
                        Some('"') => break,
                        Some(ch) => s.push(ch),
                        None => return Err("unterminated string".into()),
                    }
                }
                tokens.push(format!("\"{s}\""));
            }
            ';' => {
                // Comment: skip to end of line.
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            }
            _ => {
                let mut atom = String::new();
                while chars.peek().is_some_and(|&c| !matches!(c, ' ' | '\t' | '\r' | '\n' | '(' | ')' | '"')) {
                    atom.push(chars.next().unwrap());
                }
                tokens.push(atom);
            }
        }
    }
    Ok(tokens)
}

fn parse_tokens<'a>(tokens: &'a [String]) -> Result<(Val, &'a [String]), String> {
    if tokens.is_empty() {
        return Err("unexpected end of input".into());
    }
    if tokens[0] == "(" {
        let mut items = Vec::new();
        let mut rest = &tokens[1..];
        loop {
            if rest.is_empty() {
                return Err("unclosed parenthesis".into());
            }
            if rest[0] == ")" {
                return Ok((Val::List(items), &rest[1..]));
            }
            let (val, new_rest) = parse_tokens(rest)?;
            items.push(val);
            rest = new_rest;
        }
    } else if tokens[0] == ")" {
        Err("unexpected )".into())
    } else if tokens[0].starts_with('"') {
        let s = &tokens[0][1..tokens[0].len() - 1];
        Ok((Val::Str(s.to_string()), &tokens[1..]))
    } else if &tokens[0] == "nil" {
        Ok((Val::Nil, &tokens[1..]))
    } else {
        Ok((Val::Sym(tokens[0].clone()), &tokens[1..]))
    }
}

// ---------------------------------------------------------------------------
// Evaluator — dispatches (capability method args...) to RPC calls
// ---------------------------------------------------------------------------

struct ShellCtx {
    host: system_capnp::host::Client,
    executor: system_capnp::executor::Client,
    ipfs: ipfs_capnp::client::Client,
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
                "host"     => eval_host(args, ctx).await,
                "executor" => eval_executor(args, ctx).await,
                "ipfs"     => eval_ipfs(args, ctx).await,
                "session" => {
                    // (session <cap> <method> [args...]) — session-qualified dispatch
                    let cap = match args.first() {
                        Some(Val::Sym(s)) => s.as_str(),
                        _ => return Err("(session <capability> <method> [args...])".into()),
                    };
                    match cap {
                        "host"     => eval_host(&args[1..], ctx).await,
                        "executor" => eval_executor(&args[1..], ctx).await,
                        "ipfs"     => eval_ipfs(&args[1..], ctx).await,
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
        "connect" => {
            let addr_str = match args.get(1) {
                Some(Val::Str(s)) => s.clone(),
                _ => return Err("(host connect \"<multiaddr>\")".into()),
            };
            // Parse multiaddr: extract peer ID and address.
            // For now, pass the raw multiaddr bytes and empty peer ID.
            let mut req = ctx.host.connect_request();
            {
                let mut b = req.get();
                b.set_peer_id(&[]);
                let mut addrs = b.init_addrs(1);
                addrs.set(0, addr_str.as_bytes());
            }
            req.send()
                .promise
                .await
                .map_err(|e| e.to_string())?;
            Ok(Val::Sym("ok".into()))
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
            let exit_code = wait_resp
                .get()
                .map_err(|e| e.to_string())?
                .get_exit_code();

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
  (host id)                    Peer ID
  (host addrs)                 Listen addresses
  (host peers)                 Connected peers
  (host connect \"<multiaddr>\") Dial a peer

  (executor echo \"<msg>\")      Diagnostic echo

  (ipfs cat \"<path>\")          Fetch IPFS content
  (ipfs ls \"<path>\")           List IPFS directory

Built-ins:
  (help)                       This message
  (exit)                       Quit

Any other command is looked up in PATH as <cmd>.wasm or <cmd>/main.wasm.";

// ---------------------------------------------------------------------------
// Shell mode (TTY)
// ---------------------------------------------------------------------------

fn write_prompt(stdout: &wasip2::io::streams::OutputStream, cwd: &str) {
    let prompt = format!("{} ❯ ", cwd);
    let _ = stdout.blocking_write_and_flush(prompt.as_bytes());
}

async fn run_shell(mut ctx: ShellCtx) -> Result<(), Box<dyn std::error::Error>> {
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
                        let _ = stdout
                            .blocking_write_and_flush(format!("{result}\n").as_bytes());
                    }
                    Err(e) => {
                        let _ = stderr
                            .blocking_write_and_flush(format!("error: {e}\n").as_bytes());
                    }
                },
                Err(e) => {
                    let _ = stderr
                        .blocking_write_and_flush(format!("parse error: {e}\n").as_bytes());
                }
            }

            write_prompt(&stdout, &ctx.cwd);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Daemon mode (non-TTY)
// ---------------------------------------------------------------------------

async fn run_daemon(ctx: ShellCtx) -> Result<(), Box<dyn std::error::Error>> {
    let stderr = get_stderr();
    let stdin = get_stdin();

    // Log readiness with peer ID.
    let id_resp = ctx
        .host
        .id_request()
        .send()
        .promise
        .await
        .map_err(|e| e.to_string())?;
    let peer_id = id_resp
        .get()
        .map_err(|e| e.to_string())?
        .get_peer_id()
        .map_err(|e| e.to_string())?;
    let peer_id_hex = hex::encode(peer_id);
    let _ = stderr.blocking_write_and_flush(
        format!("{{\"event\":\"ready\",\"peer_id\":\"{peer_id_hex}\"}}\n").as_bytes(),
    );

    // Block until stdin is closed (host signals shutdown).
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

    runtime::run(|membrane: Membrane| async move {
        let graft_resp = membrane.graft_request().send().promise.await?;
        let session = graft_resp.get()?.get_session()?;

        let ctx = ShellCtx {
            host: session.get_host()?,
            executor: session.get_executor()?,
            ipfs: session.get_ipfs()?,
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
