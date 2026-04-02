//! HttpListener capability: WAGI/CGI cells served over HTTP.
//!
//! The `HttpListener` capability lets a guest register an HTTP endpoint.
//! For each incoming request matching the path prefix, the host spawns a
//! cell process (via the guest-provided `BoundExecutor`) with CGI env vars
//! as environment, request body piped to stdin, and CGI response read from stdout.
//!
//! Route registrations are stored in a shared `RouteRegistry` that the
//! `WagiService` (axum HTTP server) reads on every request. Because Cap'n
//! Proto clients are `!Send`, we use a channel-based dispatch: the axum
//! handler sends requests through an mpsc channel, and a local task on the
//! RPC event loop spawns cells and sends responses back.

use capnp::capability::Promise;
use capnp_rpc::pry;
use membrane::EpochGuard;
use tokio::sync::mpsc;

use crate::dispatcher::server::{self, CgiRequest, CgiResponse, RouteRegistry};
use crate::system_capnp;

/// Maximum response size from a cell process (16 MiB).
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub struct HttpListenerImpl {
    guard: EpochGuard,
    registry: RouteRegistry,
}

impl HttpListenerImpl {
    pub fn new(guard: EpochGuard, registry: RouteRegistry) -> Self {
        Self { guard, registry }
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::http_listener::Server for HttpListenerImpl {
    fn listen(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::http_listener::ListenParams,
        _results: system_capnp::http_listener::ListenResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());

        let reader = pry!(params.get());
        let executor = pry!(reader.get_executor());
        let prefix = pry!(pry!(reader.get_prefix()).to_str());

        // Normalize prefix: ensure it starts with /
        let prefix = if prefix.starts_with('/') {
            prefix.to_string()
        } else {
            format!("/{prefix}")
        };

        // Create a channel for the axum handler to send requests through.
        let (tx, rx) = mpsc::channel::<CgiRequest>(64);

        // Spawn a local task that receives HTTP requests from the channel,
        // spawns cells via BoundExecutor, and sends CGI responses back.
        tokio::task::spawn_local(dispatch_loop(executor, rx));

        // Register the route with its request sender.
        match self.registry.write() {
            Ok(mut routes) => {
                tracing::info!(prefix = %prefix, "registered HTTP route");
                routes.insert(prefix, tx);
                Promise::ok(())
            }
            Err(_) => Promise::err(capnp::Error::failed("route registry lock poisoned".into())),
        }
    }
}

/// Receive HTTP requests from the channel, spawn cells, send responses back.
async fn dispatch_loop(
    executor: system_capnp::bound_executor::Client,
    mut rx: mpsc::Receiver<CgiRequest>,
) {
    while let Some(req) = rx.recv().await {
        let executor = executor.clone();
        // Handle each request concurrently.
        tokio::task::spawn_local(async move {
            let response = handle_one_request(&executor, &req).await;
            let _ = req.response_tx.send(response);
        });
    }
}

/// Spawn a cell, pipe stdin/stdout, parse CGI response.
async fn handle_one_request(
    executor: &system_capnp::bound_executor::Client,
    req: &CgiRequest,
) -> CgiResponse {
    match spawn_and_run(executor, req).await {
        Ok(stdout) => match crate::dispatcher::wagi::parse_cgi_response(&stdout) {
            Ok(cgi) => CgiResponse {
                status: cgi.status_code,
                headers: cgi.headers.into_iter().collect(),
                body: cgi.body,
            },
            Err(e) => CgiResponse {
                status: 502,
                headers: vec![("content-type".to_string(), "text/plain".to_string())],
                body: format!("CGI parse error: {e}").into_bytes(),
            },
        },
        Err(e) => CgiResponse {
            status: 502,
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            body: format!("cell error: {e}").into_bytes(),
        },
    }
}

/// Spawn a cell via BoundExecutor, write body to stdin, read stdout.
async fn spawn_and_run(
    executor: &system_capnp::bound_executor::Client,
    req: &CgiRequest,
) -> Result<Vec<u8>, capnp::Error> {
    // TODO: Pass CGI env vars to the cell per-request. Currently BoundExecutor.spawn()
    // doesn't accept per-request env vars (they're bound at bind time). For WAGI, the
    // CGI env vars need to be set per request. Options:
    //   a) spawn_with_env() method on BoundExecutor
    //   b) Encode env vars as a length-prefixed header on stdin
    // For now, build the env vars (for future use) but don't pass them.
    let (server_name, server_port) = server::extract_server_info(&req.headers);
    let _env = crate::dispatcher::wagi::build_cgi_env(
        &req.method,
        &req.path,
        &req.query,
        &req.headers,
        &server_name,
        server_port,
    );

    let spawn_resp = executor.spawn_request().send().promise.await?;
    let process = spawn_resp.get()?.get_process()?;

    // Write request body to stdin, then close.
    let stdin_resp = process.stdin_request().send().promise.await?;
    let stdin = stdin_resp.get()?.get_stream()?;
    if !req.body.is_empty() {
        let mut write_req = stdin.write_request();
        write_req.get().set_data(&req.body);
        write_req.send().promise.await?;
    }
    stdin.close_request().send().promise.await?;

    // Read stdout until EOF.
    let stdout_resp = process.stdout_request().send().promise.await?;
    let stdout = stdout_resp.get()?.get_stream()?;
    let mut response = Vec::new();
    loop {
        let mut read_req = stdout.read_request();
        read_req.get().set_max_bytes(64 * 1024);
        let read_resp = read_req.send().promise.await?;
        let chunk = read_resp.get()?.get_data()?;
        if chunk.is_empty() {
            break;
        }
        response.extend_from_slice(chunk);
        if response.len() > MAX_RESPONSE_BYTES {
            let _ = process.kill_request().send().promise.await;
            return Err(capnp::Error::failed(format!(
                "cell response exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
    }

    // Collect exit code for observability.
    let wait_resp = process.wait_request().send().promise.await?;
    let exit_code = wait_resp.get()?.get_exit_code();
    if exit_code != 0 {
        tracing::warn!(exit_code, "WAGI cell exited with non-zero code");
    }

    Ok(response)
}
