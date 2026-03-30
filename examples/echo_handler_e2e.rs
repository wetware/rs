//! End-to-end integration test: HttpServer + BoundExecutor + echo cell.
//!
//! Validates the full pipeline:
//!   1. Load echo WASM binary
//!   2. Set up Cap'n Proto RPC (in-memory, no network)
//!   3. Get Executor from Host
//!   4. Executor.bind(wasm) → BoundExecutor
//!   5. BoundExecutor.spawn() → Process
//!   6. Write to Process.stdin, read from Process.stdout
//!   7. Verify echo round-trip
//!
//! Also validates HttpServer::handle() (Mode A: per-request spawn).
//!
//! Run: cargo run --example echo_handler_e2e

use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use tokio::io;
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

// Re-use the host RPC infrastructure
use ww::rpc::{build_peer_rpc, NetworkState};
use ww::system_capnp;

const ECHO_WASM: &[u8] = include_bytes!("echo/bin/echo.wasm");

/// Set up an in-memory RPC system (same pattern as rpc::tests::setup_rpc)
fn setup_rpc() -> (
    system_capnp::host::Client,
    mpsc::Receiver<ww::host::SwarmCommand>,
) {
    let (client_stream, server_stream) = io::duplex(8 * 1024);
    let (client_read, client_write) = io::split(client_stream);
    let (server_read, server_write) = io::split(server_stream);

    let peer_id = vec![1, 2, 3, 4];
    let network_state = NetworkState::from_peer_id(peer_id);
    let (swarm_tx, swarm_rx) = mpsc::channel(16);

    let server_rpc = build_peer_rpc(server_read, server_write, network_state, swarm_tx, false);
    tokio::task::spawn_local(async move {
        let _ = server_rpc.await;
    });

    let client_network = VatNetwork::new(
        client_read.compat(),
        client_write.compat_write(),
        Side::Client,
        Default::default(),
    );
    let mut client_rpc = RpcSystem::new(Box::new(client_network), None);
    let host: system_capnp::host::Client = client_rpc.bootstrap(Side::Server);
    tokio::task::spawn_local(async move {
        let _ = client_rpc.await;
    });

    (host, swarm_rx)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Initialize tracing for visibility
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            println!("Echo Cell E2E Test");
            println!("=====================\n");

            let (host, _rx) = setup_rpc();

            // Get executor from host
            let executor = host.executor_request().send().pipeline.get_executor();

            // ─── Test 1: Direct spawn via Executor.runBytes ───
            println!("--- Test 1: Direct spawn (runBytes) ---");
            {
                let mut req = executor.run_bytes_request();
                {
                    let mut b = req.get();
                    b.set_wasm(ECHO_WASM);
                }
                let resp = req.send().promise.await.unwrap();
                let process = resp.get().unwrap().get_process().unwrap();

                // Write to stdin
                let stdin_resp = process.stdin_request().send().promise.await.unwrap();
                let stdin = stdin_resp.get().unwrap().get_stream().unwrap();
                let mut write_req = stdin.write_request();
                write_req.get().set_data(b"hello from runBytes");
                write_req.send().promise.await.unwrap();
                stdin.close_request().send().promise.await.unwrap();

                // Read from stdout
                let stdout_resp = process.stdout_request().send().promise.await.unwrap();
                let stdout = stdout_resp.get().unwrap().get_stream().unwrap();
                let mut read_req = stdout.read_request();
                read_req.get().set_max_bytes(65536);
                let read_resp = read_req.send().promise.await.unwrap();
                let data = read_resp.get().unwrap().get_data().unwrap();

                assert_eq!(data, b"hello from runBytes", "runBytes echo failed");
                println!("  [OK] Echo round-trip: 'hello from runBytes'");

                // Wait for exit
                let wait_resp = process.wait_request().send().promise.await.unwrap();
                let exit_code = wait_resp.get().unwrap().get_exit_code();
                assert_eq!(exit_code, 0, "expected exit code 0");
                println!("  [OK] Exit code: {exit_code}");
            }

            // ─── Test 2: BoundExecutor.bind() + spawn() ───
            println!("\n--- Test 2: BoundExecutor (bind + spawn) ---");
            {
                let mut bind_req = executor.bind_request();
                {
                    let mut b = bind_req.get();
                    b.set_wasm(ECHO_WASM);
                }
                let bind_resp = bind_req.send().promise.await.unwrap();
                let bound = bind_resp.get().unwrap().get_bound().unwrap();

                // Spawn first instance
                let spawn_resp = bound.spawn_request().send().promise.await.unwrap();
                let process = spawn_resp.get().unwrap().get_process().unwrap();

                let stdin_resp = process.stdin_request().send().promise.await.unwrap();
                let stdin = stdin_resp.get().unwrap().get_stream().unwrap();
                let mut write_req = stdin.write_request();
                write_req.get().set_data(b"hello from BoundExecutor");
                write_req.send().promise.await.unwrap();
                stdin.close_request().send().promise.await.unwrap();

                let stdout_resp = process.stdout_request().send().promise.await.unwrap();
                let stdout = stdout_resp.get().unwrap().get_stream().unwrap();
                let mut read_req = stdout.read_request();
                read_req.get().set_max_bytes(65536);
                let read_resp = read_req.send().promise.await.unwrap();
                let data = read_resp.get().unwrap().get_data().unwrap();

                assert_eq!(data, b"hello from BoundExecutor");
                println!("  [OK] Echo round-trip: 'hello from BoundExecutor'");

                let wait_resp = process.wait_request().send().promise.await.unwrap();
                assert_eq!(wait_resp.get().unwrap().get_exit_code(), 0);
                println!("  [OK] Exit code: 0");

                // Spawn second instance (verifies BoundExecutor is reusable)
                let spawn_resp2 = bound.spawn_request().send().promise.await.unwrap();
                let process2 = spawn_resp2.get().unwrap().get_process().unwrap();

                let stdin_resp2 = process2.stdin_request().send().promise.await.unwrap();
                let stdin2 = stdin_resp2.get().unwrap().get_stream().unwrap();
                let mut write_req2 = stdin2.write_request();
                write_req2.get().set_data(b"second spawn");
                write_req2.send().promise.await.unwrap();
                stdin2.close_request().send().promise.await.unwrap();

                let stdout_resp2 = process2.stdout_request().send().promise.await.unwrap();
                let stdout2 = stdout_resp2.get().unwrap().get_stream().unwrap();
                let mut read_req2 = stdout2.read_request();
                read_req2.get().set_max_bytes(65536);
                let read_resp2 = read_req2.send().promise.await.unwrap();
                let data2 = read_resp2.get().unwrap().get_data().unwrap();

                assert_eq!(data2, b"second spawn");
                println!("  [OK] Second spawn echo: 'second spawn'");

                let wait_resp2 = process2.wait_request().send().promise.await.unwrap();
                assert_eq!(wait_resp2.get().unwrap().get_exit_code(), 0);
                println!("  [OK] Second spawn exit code: 0");
            }

            // ─── Test 3: HttpServer.handle() (Mode A) ───
            println!("\n--- Test 3: HttpServer.handle() (per-request spawn) ---");
            {
                let mut bind_req = executor.bind_request();
                bind_req.get().set_wasm(ECHO_WASM);
                let bind_resp = bind_req.send().promise.await.unwrap();
                let bound = bind_resp.get().unwrap().get_bound().unwrap();

                let server = ww::dispatcher::HttpServer::new(bound);

                let (response, exit_code) = server
                    .handle(b"hello via HttpServer".to_vec())
                    .await
                    .unwrap();

                assert_eq!(response, b"hello via HttpServer");
                assert_eq!(exit_code, 0);
                println!("  [OK] HttpServer echo: 'hello via HttpServer'");
                println!("  [OK] Exit code: {exit_code}");

                // Second request (new spawn)
                let (response2, exit_code2) =
                    server.handle(b"second request".to_vec()).await.unwrap();
                assert_eq!(response2, b"second request");
                assert_eq!(exit_code2, 0);
                println!("  [OK] Second request echo: 'second request'");
            }

            println!("\n=====================");
            println!("ALL TESTS PASSED");
        })
        .await;
}
