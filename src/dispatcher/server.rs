//! WagiService: axum HTTP server on a dedicated OS thread.
//!
//! Implements the `Service` trait from `runtime.rs`. Accepts route
//! registrations from `HttpListenerImpl` via a shared registry, then
//! dispatches incoming HTTP requests to WASM cells using the CGI adapter
//! (`dispatcher::wagi`).
//!
//! Architecture: Cap'n Proto clients are `!Send`, so the axum handler
//! can't hold a `Executor` directly. Instead, each route registers
//! a `RequestSender` (an mpsc channel). The axum handler sends requests
//! through the channel, and a local task on the RPC event loop receives
//! them, spawns cells via `Executor`, and sends responses back.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use tokio::sync::{mpsc, oneshot, watch};

/// An HTTP request to be dispatched to a WASM cell.
pub struct CgiRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// JFS-verified `X-Snap-Payload` data, if the request carried a
    /// valid one. `None` means no header, or verification failed
    /// (currently logged-warn-and-drop in v1.0; v1.1 will return 4xx
    /// per spec). Cells consume this through CGI env vars emitted by
    /// `dispatcher::wagi::build_cgi_env`.
    pub verified_snap: Option<crate::jfs::VerifiedJfs>,
    pub response_tx: oneshot::Sender<CgiResponse>,
}

/// An HTTP response from a WASM cell.
pub struct CgiResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Sender half of the request channel. Stored in the route registry.
/// `Send + Sync` because `mpsc::Sender` is `Send + Sync`.
pub type RequestSender = mpsc::Sender<CgiRequest>;

/// Shared route registry: path prefix → request channel sender.
pub type RouteRegistry = Arc<RwLock<HashMap<String, RequestSender>>>;

/// Create a new empty route registry.
pub fn new_registry() -> RouteRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

/// The axum HTTP server running on its own OS thread.
///
/// ```text
/// Host supervisor
///  ├── Thread: SwarmService
///  ├── Thread: EpochService
///  ├── Thread: WagiService  ← this
///  └── Threads: ExecutorPool
/// ```
pub struct WagiService {
    pub listen_addr: std::net::SocketAddr,
    pub registry: RouteRegistry,
}

impl crate::runtime::Service for WagiService {
    fn run(self, mut shutdown: watch::Receiver<()>) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _span = tracing::info_span!("wagi-http").entered();

        rt.block_on(async move {
            let app = Router::new()
                .route("/{*path}", any(handle_request))
                .route("/", any(handle_request))
                .with_state(self.registry);

            let listener = tokio::net::TcpListener::bind(self.listen_addr).await?;
            let local_addr = listener.local_addr()?;
            tracing::info!(%local_addr, "WAGI HTTP server listening");

            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown.changed().await;
                    tracing::info!("WAGI HTTP server shutting down");
                })
                .await?;

            Ok(())
        })
    }
}

/// Maximum request body size (16 MiB).
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// Axum handler: match path prefix, send request through channel, await response.
async fn handle_request(State(registry): State<RouteRegistry>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let method = request.method().to_string();

    // Find the longest matching prefix in the registry.
    let sender = {
        let routes = match registry.read() {
            Ok(r) => r,
            Err(_) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "route lock poisoned")
            }
        };
        find_longest_prefix(&routes, &path)
    };

    let sender = match sender {
        Some(s) => s,
        None => return error_response(StatusCode::NOT_FOUND, &format!("no handler for {path}")),
    };

    // Extract headers before consuming the body.
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    // Best-effort JFS verification of `X-Snap-Payload`. v1.0 is
    // permissive: success populates `verified_snap`; failure logs a
    // warning and treats the request as anonymous. v1.1 will follow
    // the spec strictly (`MUST reject malformed/expired/invalid with
    // 4xx`) once Hub key verification ships alongside.
    let verified_snap = verify_snap_payload(&headers);

    // Read the request body.
    let body_bytes = match axum::body::to_bytes(request.into_body(), MAX_REQUEST_BYTES).await {
        Ok(b) => b.to_vec(),
        Err(_) => return error_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };

    // Send request to the RPC event loop and await response.
    let (response_tx, response_rx) = oneshot::channel();
    let cgi_req = CgiRequest {
        method,
        path,
        query,
        headers,
        body: body_bytes,
        verified_snap,
        response_tx,
    };

    if sender.send(cgi_req).await.is_err() {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "route handler closed");
    }

    match response_rx.await {
        Ok(resp) => build_http_response(&resp),
        Err(_) => error_response(StatusCode::BAD_GATEWAY, "cell handler dropped response"),
    }
}

/// Find the longest prefix match in the route table.
fn find_longest_prefix(
    routes: &HashMap<String, RequestSender>,
    path: &str,
) -> Option<RequestSender> {
    let mut best: Option<(&str, &RequestSender)> = None;
    for (prefix, sender) in routes {
        if path.starts_with(prefix.as_str()) {
            match best {
                Some((current_best, _)) if prefix.len() <= current_best.len() => {}
                _ => best = Some((prefix.as_str(), sender)),
            }
        }
    }
    best.map(|(_, sender)| sender.clone())
}

/// Build an axum Response from a CgiResponse.
fn build_http_response(cgi: &CgiResponse) -> Response {
    let mut builder = Response::builder().status(cgi.status);
    for (key, value) in &cgi.headers {
        builder = builder.header(key.as_str(), value.as_str());
    }
    builder
        .body(Body::from(cgi.body.clone()))
        .unwrap_or_else(|_| {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "response build error")
        })
}

/// Build an error response.
fn error_response(status: StatusCode, msg: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Body::from(msg.to_string()))
        .unwrap()
}

/// Extract server name and port from Host header.
pub fn extract_server_info(headers: &[(String, String)]) -> (String, u16) {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("host") {
            if let Some(colon) = value.rfind(':') {
                let host = &value[..colon];
                let port = value[colon + 1..].parse().unwrap_or(80);
                return (host.to_string(), port);
            }
            return (value.clone(), 80);
        }
    }
    ("localhost".to_string(), 80)
}

/// Find a header by name (case-insensitive). Returns the first match.
fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Build the audience string this server expects in the JFS payload.
///
/// `audience` per spec is the server origin: `scheme + host + port`
/// (with port omitted if it's the default for the scheme). We derive
/// it from the `Host` header + `X-Forwarded-Proto` (Traefik / any TLS
/// terminator in front sets this); if `X-Forwarded-Proto` is absent
/// we default to `https` because the production listener is intended
/// to live behind TLS termination.
///
/// Returns `None` if there's no `Host` header (request wasn't HTTP/1.1
/// or HTTP/2 in any normal sense — bail).
fn derive_audience(headers: &[(String, String)]) -> Option<String> {
    let host = find_header(headers, "host")?;
    let scheme = find_header(headers, "x-forwarded-proto").unwrap_or("https");
    Some(format!("{scheme}://{host}"))
}

/// Best-effort JFS verification of an `X-Snap-Payload` header.
///
/// v1.0 contract: success → `Some(VerifiedJfs)`; absent header → `None`;
/// any verification failure → `None` + `WARN` log. v1.1 will follow
/// the spec's `MUST reject 4xx on malformed/expired/invalid` once Hub
/// key verification ships in lockstep.
fn verify_snap_payload(headers: &[(String, String)]) -> Option<crate::jfs::VerifiedJfs> {
    let payload = find_header(headers, "x-snap-payload")?;
    let audience = match derive_audience(headers) {
        Some(a) => a,
        None => {
            tracing::warn!("X-Snap-Payload present but Host header missing — skipping JFS verify");
            return None;
        }
    };
    let now = chrono::Utc::now().timestamp();
    match crate::jfs::verify(
        payload,
        &audience,
        now,
        crate::jfs::DEFAULT_TIMESTAMP_SKEW_SECS,
    ) {
        Ok(v) => {
            tracing::debug!(
                fid = v.payload.fid,
                "X-Snap-Payload verified (FID claimed, NOT Hub-verified)"
            );
            Some(v)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "X-Snap-Payload failed verification; treating request as anonymous (v1.0 permissive; v1.1 will reject 4xx per spec)"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_longest_prefix_empty_returns_none() {
        let routes = HashMap::new();
        assert!(find_longest_prefix(&routes, "/api/v1/prices").is_none());
    }

    #[test]
    fn extract_server_info_parses_host_header() {
        let headers = vec![("Host".to_string(), "example.com:8080".to_string())];
        let (name, port) = extract_server_info(&headers);
        assert_eq!(name, "example.com");
        assert_eq!(port, 8080);
    }

    #[test]
    fn extract_server_info_default_port() {
        let headers = vec![("host".to_string(), "example.com".to_string())];
        let (name, port) = extract_server_info(&headers);
        assert_eq!(name, "example.com");
        assert_eq!(port, 80);
    }

    #[test]
    fn extract_server_info_no_host() {
        let headers: Vec<(String, String)> = vec![];
        let (name, port) = extract_server_info(&headers);
        assert_eq!(name, "localhost");
        assert_eq!(port, 80);
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = new_registry();
        assert!(reg.read().unwrap().is_empty());
    }
}
