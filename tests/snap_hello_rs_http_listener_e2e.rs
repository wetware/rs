//! End-to-end test for the snap-hello-rs cell via the HttpListener
//! dispatch chain.
//!
//! Mirrors `tests/status_cell_http_listener_e2e.rs`. Validates the full
//! Farcaster Snap content-negotiation contract:
//!
//!   GET with `Accept: application/vnd.farcaster.snap+json`
//!     → 200, snap-JSON body, snap content-type, Vary/Cache-Control/ACAO headers
//!
//!   GET with `Accept: text/html` (or anything else)
//!     → 200, HTML body, text/html content-type, Link rel=alternate header
//!
//! Spec: https://docs.farcaster.xyz/snap/spec-overview
//!       https://docs.farcaster.xyz/snap/http-headers
//!
//! Requires pre-built snap WASM: `make -C examples/snap-hello-rs`.
//! No graft caps used; the cell is stateless.

use tokio::sync::{mpsc, oneshot, watch};

use ww::dispatcher::server::{new_registry, CgiRequest, CgiResponse};
use ww::rpc::{create_runtime_client, CachePolicy, NetworkState};
use ww::system_capnp;

const SNAP_WASM_PATH: &str = "examples/snap-hello-rs/bin/snap-hello-rs.wasm";
const SNAP_TYPE: &str = "application/vnd.farcaster.snap+json";

fn snap_wasm_exists() -> bool {
    std::path::Path::new(SNAP_WASM_PATH).exists()
}

fn synth_peer_id_bytes() -> Vec<u8> {
    let kp = libp2p::identity::Keypair::generate_ed25519();
    libp2p::PeerId::from_public_key(&kp.public()).to_bytes()
}

/// Case-insensitive header lookup. Returns the first match.
fn find_header<'a>(resp: &'a CgiResponse, name: &str) -> Option<&'a str> {
    resp.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Set up Runtime + HttpListener wiring, register the snap cell on
/// `/snaps/hello`, and return the request-sender for that route.
/// Lives inside a `LocalSet` because capnp-rpc spawns `!Send` tasks.
async fn register_snap_route() -> mpsc::Sender<CgiRequest> {
    let network_state = NetworkState::new();
    let peer_id_bytes = synth_peer_id_bytes();
    network_state.set_local_peer_id(peer_id_bytes).await;

    let epoch = membrane::Epoch {
        seq: 1,
        head: vec![],
        provenance: membrane::Provenance::Block(0),
    };
    let (_epoch_tx, epoch_rx) = watch::channel(epoch);
    let guard = membrane::EpochGuard {
        issued_seq: 1,
        receiver: epoch_rx.clone(),
    };
    let stream_control = libp2p_stream::Behaviour::new().new_control();

    let (swarm_tx, _swarm_rx) = mpsc::channel(16);
    let runtime = create_runtime_client(
        network_state,
        swarm_tx,
        false,
        Some(guard.clone()),
        Some(epoch_rx.clone()),
        None,
        Some(stream_control),
        CachePolicy::Shared,
        ww::ipfs::HttpClient::new("http://localhost:5001".into()),
        Vec::new(),
    );

    let wasm = std::fs::read(SNAP_WASM_PATH).expect("read snap-hello-rs.wasm");
    let mut load_req = runtime.load_request();
    load_req.get().set_wasm(&wasm);
    let load_resp = load_req.send().promise.await.expect("runtime.load");
    let executor = load_resp
        .get()
        .expect("load resp")
        .get_executor()
        .expect("get executor");

    let route_registry = new_registry();
    let listener_impl =
        ww::rpc::http_listener::HttpListenerImpl::new(guard, route_registry.clone());
    let listener: system_capnp::http_listener::Client = capnp_rpc::new_client(listener_impl);

    let mut listen_req = listener.listen_request();
    listen_req.get().set_executor(executor);
    listen_req.get().set_prefix("/snaps/hello");
    // No caps grafted: this cell is stateless and pure.
    let _ = listen_req.get().init_caps(0);
    listen_req
        .send()
        .promise
        .await
        .expect("HttpListener.listen should succeed");

    let routes = route_registry.read().expect("registry read lock");
    routes
        .get("/snaps/hello")
        .cloned()
        .expect("route /snaps/hello should be registered")
}

/// Send a CGI request with the given headers through the route channel
/// and await the response (with timeout).
async fn dispatch(tx: &mpsc::Sender<CgiRequest>, headers: Vec<(String, String)>) -> CgiResponse {
    let (response_tx, response_rx) = oneshot::channel();
    let cgi_req = CgiRequest {
        method: "GET".into(),
        path: "/snaps/hello".into(),
        query: String::new(),
        headers,
        body: Vec::new(),
        response_tx,
    };
    tx.send(cgi_req)
        .await
        .expect("CgiRequest should send through route channel");

    tokio::time::timeout(std::time::Duration::from_secs(20), response_rx)
        .await
        .expect("dispatch should respond within 20s")
        .expect("response_rx not dropped")
}

/// Snap-aware client: Accept header signals snap support.
/// Cell must return snap-JSON with all 4 spec-required headers.
#[tokio::test(flavor = "current_thread")]
async fn snap_cell_with_snap_accept_returns_snap_json_and_required_headers() {
    if !snap_wasm_exists() {
        eprintln!("skipping: {SNAP_WASM_PATH} not built (run `make -C examples/snap-hello-rs` first)");
        return;
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let tx = register_snap_route().await;
            let resp = dispatch(&tx, vec![("Accept".into(), SNAP_TYPE.into())]).await;

            assert_eq!(resp.status, 200, "expected HTTP 200");

            // Headers required by the snap spec.
            assert_eq!(
                find_header(&resp, "Content-Type"),
                Some(SNAP_TYPE),
                "Content-Type must be the snap media type"
            );
            assert_eq!(
                find_header(&resp, "Vary"),
                Some("Accept"),
                "Vary: Accept is required by spec content-negotiation contract"
            );
            assert_eq!(
                find_header(&resp, "Cache-Control"),
                Some("public, max-age=300"),
                "Cache-Control should match the documented posture"
            );
            assert_eq!(
                find_header(&resp, "Access-Control-Allow-Origin"),
                Some("*"),
                "ACAO should be open"
            );

            // Body validates against the Farcaster Snap response shape.
            let body = std::str::from_utf8(&resp.body).expect("UTF-8 body");
            let json: serde_json::Value = serde_json::from_str(body)
                .unwrap_or_else(|e| panic!("response should parse as JSON: {e}\nbody: {body}"));
            assert_eq!(json["version"], "2.0");
            assert_eq!(json["ui"]["root"], "greeting");
            assert_eq!(json["ui"]["elements"]["greeting"]["type"], "text");
            assert_eq!(
                json["ui"]["elements"]["greeting"]["props"]["content"],
                "Hello, @stranger"
            );
        })
        .await;
}

/// Plain browser / link previewer / crawler: no snap Accept.
/// Cell must return HTML + a `Link rel="alternate"` header pointing
/// at the snap representation. Spec citizenship per /snap/http-headers.
#[tokio::test(flavor = "current_thread")]
async fn snap_cell_without_snap_accept_returns_html_with_link_alternate() {
    if !snap_wasm_exists() {
        eprintln!("skipping: {SNAP_WASM_PATH} not built (run `make -C examples/snap-hello-rs` first)");
        return;
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let tx = register_snap_route().await;
            let resp = dispatch(&tx, vec![("Accept".into(), "text/html".into())]).await;

            assert_eq!(resp.status, 200, "expected HTTP 200");

            // HTML posture's required headers.
            let ct = find_header(&resp, "Content-Type").expect("Content-Type missing");
            assert!(
                ct.starts_with("text/html"),
                "Content-Type should be text/html, got {ct:?}"
            );
            assert_eq!(find_header(&resp, "Vary"), Some("Accept"));
            assert_eq!(
                find_header(&resp, "Cache-Control"),
                Some("public, max-age=300")
            );
            assert_eq!(find_header(&resp, "Access-Control-Allow-Origin"), Some("*"));

            // The Link header is the protocol-discovery hook for snap-aware
            // clients fetching with a non-snap Accept. Must point at the snap
            // type (empty `<>` resolves to current URL per RFC 3986).
            let link = find_header(&resp, "Link").expect("Link header missing");
            assert!(
                link.contains("rel=\"alternate\""),
                "Link must declare rel=alternate, got {link:?}"
            );
            assert!(
                link.contains(SNAP_TYPE),
                "Link must reference the snap media type, got {link:?}"
            );

            // Body is HTML and mentions the snap title (lightly — we don't
            // overfit to the exact placeholder text).
            let body = std::str::from_utf8(&resp.body).expect("UTF-8 body");
            assert!(
                body.contains("<!DOCTYPE html>"),
                "HTML fallback body should be a real HTML document"
            );
        })
        .await;
}

/// Empty Accept header (some bare crawlers, naked curl) — same as
/// no-snap-Accept path. This isn't a separate posture; it's a sanity
/// check that the negotiation defaults to the HTML fallback rather
/// than panicking or returning snap-JSON to a non-Farcaster client.
#[tokio::test(flavor = "current_thread")]
async fn snap_cell_with_empty_accept_returns_html_fallback() {
    if !snap_wasm_exists() {
        eprintln!("skipping: {SNAP_WASM_PATH} not built (run `make -C examples/snap-hello-rs` first)");
        return;
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let tx = register_snap_route().await;
            let resp = dispatch(&tx, Vec::new()).await;

            assert_eq!(resp.status, 200);
            let ct = find_header(&resp, "Content-Type").expect("Content-Type missing");
            assert!(
                ct.starts_with("text/html"),
                "empty Accept should default to HTML fallback, got {ct:?}"
            );
            assert!(find_header(&resp, "Link").is_some(), "Link header expected");
        })
        .await;
}
