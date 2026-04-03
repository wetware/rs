//! E2E tests for the shell cell.
//!
//! Uses `handle_vat_connection_spawn` with an in-memory duplex to spawn
//! a real shell cell process, bootstraps a Shell client, and exercises
//! eval + state persistence + error handling.
//!
//! Requires pre-built WASM: `make shell`

use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use tokio::sync::mpsc;
use tokio_util::compat::TokioAsyncReadCompatExt;

use ww::rpc::{create_runtime_client, CachePolicy, NetworkState};
use ww::shell_capnp;
use ww::system_capnp;

const SHELL_WASM_PATH: &str = "std/shell/boot/main.wasm";

fn load_wasm(path: &str) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

fn setup_runtime() -> system_capnp::runtime::Client {
    let network_state = NetworkState::new();
    let (swarm_tx, _swarm_rx) = mpsc::channel(16);

    // Shell cell needs a membrane (system::serve grafts capabilities).
    // Provide epoch_rx and a stream_control so ExecutorImpl takes the
    // build_membrane_rpc path instead of build_peer_rpc.
    let epoch = membrane::Epoch {
        seq: 1,
        head: vec![],
        adopted_block: 0,
    };
    let (epoch_tx, epoch_rx) = tokio::sync::watch::channel(epoch);
    let _ = epoch_tx; // keep alive

    // Create a minimal stream behaviour for stream_control.
    let stream_behaviour = libp2p_stream::Behaviour::new();
    let stream_control = stream_behaviour.new_control();
    // The behaviour needs to be driven, but for tests we just need the control handle.
    // Leak the behaviour since it's test-only.
    Box::leak(Box::new(stream_behaviour));

    let guard = membrane::EpochGuard {
        issued_seq: 1,
        receiver: epoch_rx.clone(),
    };

    create_runtime_client(
        network_state,
        swarm_tx,
        false,
        Some(guard),
        Some(epoch_rx),
        None, // no signing key
        Some(stream_control),
        CachePolicy::Shared,
    )
}

/// Helper: spawn a shell cell via duplex and return a Shell client.
async fn spawn_shell_client(
    wasm: &[u8],
    runtime: &system_capnp::runtime::Client,
) -> (
    shell_capnp::shell::Client,
    tokio::task::JoinHandle<Result<(), capnp::Error>>,
    tokio::task::JoinHandle<()>,
) {
    let (peer_stream, bridge_stream) = tokio::io::duplex(64 * 1024);

    let wasm = wasm.to_vec();
    let runtime = runtime.clone();
    let bridge_handle = tokio::task::spawn_local(async move {
        let mut load_req = runtime.load_request();
        load_req.get().set_wasm(&wasm);
        let load_resp = load_req.send().promise.await.unwrap();
        let executor = load_resp.get().unwrap().get_executor().unwrap();
        ww::rpc::vat_listener::handle_vat_connection_spawn(
            executor,
            Vec::new(),
            bridge_stream.compat(),
            "test-shell-cid",
        )
        .await
    });

    let (peer_read, peer_write) = tokio::io::split(peer_stream);
    let peer_network = VatNetwork::new(
        peer_read.compat(),
        tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(peer_write),
        Side::Client,
        Default::default(),
    );
    let mut peer_rpc = RpcSystem::new(Box::new(peer_network), None);
    let shell: shell_capnp::shell::Client = peer_rpc.bootstrap(Side::Server);
    let rpc_handle = tokio::task::spawn_local(async move {
        let _ = peer_rpc.await;
    });

    // Give the cell time to bootstrap. The serve closure grafts the membrane
    // and loads the prelude. We wait by polling with a nil eval.
    // Note: if the cell crashes, the peer gets Disconnected errors.

    (shell, bridge_handle, rpc_handle)
}

/// Wait for the shell cell to finish initializing by polling with empty evals.
async fn wait_ready(shell: &shell_capnp::shell::Client) {
    for i in 0..100 {
        let mut req = shell.eval_request();
        req.get().set_text("nil");
        match tokio::time::timeout(std::time::Duration::from_secs(5), req.send().promise).await {
            Ok(Ok(resp)) => {
                let result = resp.get().unwrap();
                let text = result.get_result().unwrap().to_str().unwrap_or("");
                let is_error = result.get_is_error();
                if !is_error || !text.contains("not ready") {
                    eprintln!("  shell ready after {i} polls");
                    return; // Ready!
                }
                eprintln!("  poll {i}: not ready yet");
            }
            Ok(Err(e)) => {
                eprintln!("  poll {i}: RPC error: {e}");
                // Cell may have crashed — keep trying for a bit.
            }
            Err(_) => {
                eprintln!("  poll {i}: timeout");
            }
        }
    }
    panic!("shell cell never became ready");
}

/// Helper: eval a Glia expression and return (result_text, is_error).
async fn eval(shell: &shell_capnp::shell::Client, text: &str) -> (String, bool) {
    let mut req = shell.eval_request();
    req.get().set_text(text);
    let resp = tokio::time::timeout(std::time::Duration::from_secs(15), req.send().promise)
        .await
        .expect("eval timed out")
        .expect("eval RPC failed");
    let result = resp.get().unwrap();
    let text = result
        .get_result()
        .unwrap()
        .to_str()
        .unwrap_or("(invalid UTF-8)")
        .to_string();
    let is_error = result.get_is_error();
    (text, is_error)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_shell_eval_arithmetic() {
    let wasm = match load_wasm(SHELL_WASM_PATH) {
        Some(w) => w,
        None => {
            eprintln!("SKIP: shell WASM not built (run `make shell`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let runtime = setup_runtime();
            let (shell, _bridge, _rpc) = spawn_shell_client(&wasm, &runtime).await;

            // First: wait for shell to be ready
            wait_ready(&shell).await;

            let (result, is_error) = eval(&shell, "(+ 1 2)").await;
            assert!(!is_error, "arithmetic should not error: {result}");
            assert_eq!(result, "3");
        })
        .await;
}

#[tokio::test]
async fn test_shell_eval_state_persistence() {
    let wasm = match load_wasm(SHELL_WASM_PATH) {
        Some(w) => w,
        None => {
            eprintln!("SKIP: shell WASM not built (run `make shell`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let runtime = setup_runtime();
            let (shell, _bridge, _rpc) = spawn_shell_client(&wasm, &runtime).await;

            // def should persist across evals
            let (_, is_error) = eval(&shell, "(def x 42)").await;
            assert!(!is_error, "def should not error");

            let (result, is_error) = eval(&shell, "x").await;
            assert!(!is_error, "x lookup should not error: {result}");
            assert_eq!(result, "42");
        })
        .await;
}

#[tokio::test]
async fn test_shell_eval_parse_error() {
    let wasm = match load_wasm(SHELL_WASM_PATH) {
        Some(w) => w,
        None => {
            eprintln!("SKIP: shell WASM not built (run `make shell`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let runtime = setup_runtime();
            let (shell, _bridge, _rpc) = spawn_shell_client(&wasm, &runtime).await;

            let (result, is_error) = eval(&shell, "(+ 1").await;
            assert!(is_error, "unmatched paren should be a parse error");
            assert!(
                result.contains("parse error"),
                "should mention parse error: {result}"
            );
        })
        .await;
}

#[tokio::test]
async fn test_shell_eval_unknown_symbol() {
    let wasm = match load_wasm(SHELL_WASM_PATH) {
        Some(w) => w,
        None => {
            eprintln!("SKIP: shell WASM not built (run `make shell`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let runtime = setup_runtime();
            let (shell, _bridge, _rpc) = spawn_shell_client(&wasm, &runtime).await;

            let (result, is_error) = eval(&shell, "nonexistent_symbol").await;
            assert!(is_error, "unknown symbol should be an error: {result}");
        })
        .await;
}

#[tokio::test]
async fn test_shell_eval_exit_sentinel() {
    let wasm = match load_wasm(SHELL_WASM_PATH) {
        Some(w) => w,
        None => {
            eprintln!("SKIP: shell WASM not built (run `make shell`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let runtime = setup_runtime();
            let (shell, _bridge, _rpc) = spawn_shell_client(&wasm, &runtime).await;

            let (result, is_error) = eval(&shell, "(exit)").await;
            assert!(!is_error, "(exit) should not be an error");
            assert_eq!(result, ":exit", "(exit) should return :exit keyword");
        })
        .await;
}

#[tokio::test]
async fn test_shell_eval_empty_input() {
    let wasm = match load_wasm(SHELL_WASM_PATH) {
        Some(w) => w,
        None => {
            eprintln!("SKIP: shell WASM not built (run `make shell`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let runtime = setup_runtime();
            let (shell, _bridge, _rpc) = spawn_shell_client(&wasm, &runtime).await;

            let (result, is_error) = eval(&shell, "").await;
            assert!(!is_error, "empty input should not error");
            assert!(result.is_empty(), "empty input should return empty result");
        })
        .await;
}

#[tokio::test]
async fn test_shell_eval_prelude_macros() {
    let wasm = match load_wasm(SHELL_WASM_PATH) {
        Some(w) => w,
        None => {
            eprintln!("SKIP: shell WASM not built (run `make shell`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let runtime = setup_runtime();
            let (shell, _bridge, _rpc) = spawn_shell_client(&wasm, &runtime).await;

            // `when` is a prelude macro
            let (result, is_error) = eval(&shell, "(when true 42)").await;
            assert!(!is_error, "prelude macro should work: {result}");
            assert_eq!(result, "42");

            // `defn` is a prelude macro
            let (_, is_error) = eval(&shell, "(defn double [x] (* x 2))").await;
            assert!(!is_error, "defn should not error");

            let (result, is_error) = eval(&shell, "(double 21)").await;
            assert!(!is_error, "calling defn'd function should work: {result}");
            assert_eq!(result, "42");
        })
        .await;
}
