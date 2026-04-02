//! Integration test: discovery cell spawn + Greeter RPC round-trip.
//!
//! Validates the host-side chain that VatListener uses internally:
//!   executor.runBytes(wasm) → process.bootstrap() → Greeter cap → greet()
//!
//! No args = cell mode (default). No libp2p networking required.
//! Uses in-memory RPC over duplex streams.
//!
//! Requires a pre-built discovery WASM at `examples/discovery/bin/discovery.wasm`.
//! Build:  make discovery

use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use membrane::Epoch;
use ww::greeter_capnp;
use ww::ipfs::MemoryStore;
use ww::rpc::{ExecutorImpl, NetworkState};
use ww::system_capnp;

const DISCOVERY_WASM_PATH: &str = "examples/discovery/bin/discovery.wasm";

/// Skip the test if the WASM binary hasn't been built.
fn load_discovery_wasm() -> Option<Vec<u8>> {
    std::fs::read(DISCOVERY_WASM_PATH).ok()
}

/// Create an Executor with full membrane support so spawned cells
/// get a Membrane bootstrap (required for system::serve() export).
fn setup_executor() -> system_capnp::executor::Client {
    let network_state = NetworkState::new();
    let (swarm_tx, _swarm_rx) = mpsc::channel(16);
    let epoch = Epoch {
        seq: 1,
        head: Vec::new(),
        adopted_block: 0,
    };
    let (_epoch_tx, epoch_rx) = watch::channel(epoch);
    let content_store: Arc<dyn ww::ipfs::ContentStore> = Arc::new(MemoryStore::new());
    let stream_control = libp2p_stream::Behaviour::new().new_control();

    let executor = ExecutorImpl::new_full(
        network_state,
        swarm_tx,
        false,
        None, // no epoch guard (testing, not production)
        Some(epoch_rx),
        Some(content_store),
        None, // no signing key
        Some(stream_control),
    );

    capnp_rpc::new_client(executor)
}

/// Spawn the discovery WASM in cell mode and get its bootstrap Greeter cap.
async fn spawn_greeter_cell(
    executor: &system_capnp::executor::Client,
    wasm: &[u8],
) -> greeter_capnp::greeter::Client {
    let mut req = executor.run_bytes_request();
    req.get().set_wasm(wasm);
    {
        // No args = cell mode (default). No WW_CELL envvar needed.
        let mut env = req.get().init_env(1);
        env.set(0, "WW_PEER_ID=deadbeefcafebabe");
    }
    let resp = req.send().promise.await.expect("runBytes failed");
    let process = resp.get().unwrap().get_process().unwrap();

    // Get the cell's exported bootstrap capability (with timeout).
    let bootstrap_resp = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        process.bootstrap_request().send().promise,
    )
    .await
    .expect("cell did not export bootstrap within 10s")
    .expect("bootstrap RPC failed");

    bootstrap_resp
        .get()
        .unwrap()
        .get_cap()
        .get_as_capability()
        .expect("failed to cast bootstrap cap to Greeter")
}

#[tokio::test]
async fn test_discovery_cell_greet() {
    let wasm = match load_discovery_wasm() {
        Some(w) => w,
        None => {
            eprintln!("SKIP: discovery WASM not built (run `make discovery`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let executor = setup_executor();
            let greeter = spawn_greeter_cell(&executor, &wasm).await;

            // Call greet() and verify the response.
            let mut req = greeter.greet_request();
            req.get().set_name("integration-test");
            let resp = req.send().promise.await.expect("greet RPC failed");
            let greeting = resp
                .get()
                .unwrap()
                .get_greeting()
                .unwrap()
                .to_str()
                .unwrap();

            assert!(
                greeting.contains("Hello, integration-test!"),
                "unexpected greeting: {greeting}"
            );
            assert!(
                greeting.contains("I'm"),
                "greeting should include peer identity: {greeting}"
            );
            // The peer ID we passed was "deadbeefcafebabe" (hex),
            // so short_id should show the last 8 hex chars.
            assert!(
                greeting.contains("cafebabe"),
                "greeting should contain short peer ID: {greeting}"
            );
        })
        .await;
}

#[tokio::test]
async fn test_discovery_cell_greet_multiple() {
    let wasm = match load_discovery_wasm() {
        Some(w) => w,
        None => {
            eprintln!("SKIP: discovery WASM not built (run `make discovery`)");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let executor = setup_executor();
            let greeter = spawn_greeter_cell(&executor, &wasm).await;

            // Multiple calls on the same cell should all succeed.
            for name in &["Alice", "Bob", "Charlie"] {
                let mut req = greeter.greet_request();
                req.get().set_name(name);
                let resp = req.send().promise.await.expect("greet RPC failed");
                let greeting = resp
                    .get()
                    .unwrap()
                    .get_greeting()
                    .unwrap()
                    .to_str()
                    .unwrap();

                assert!(
                    greeting.contains(&format!("Hello, {name}!")),
                    "unexpected greeting for {name}: {greeting}"
                );
            }
        })
        .await;
}
