//! Integration test: discovery cell spawn + Greeter RPC round-trip.
//!
//! Validates the host-side chain that VatListener uses internally:
//!   runtime.load(wasm) → executor.spawn() → process.bootstrap() → Greeter cap → greet()
//!
//! No args = cell mode (default). No libp2p networking required.
//! Uses in-memory RPC over duplex streams.
//!
//! Requires a pre-built discovery WASM at `examples/discovery/bin/discovery.wasm`.
//! Build:  make discovery

use tokio::sync::mpsc;

use ww::greeter_capnp;
use ww::rpc::{create_runtime_client, CachePolicy, NetworkState};
use ww::system_capnp;

const DISCOVERY_WASM_PATH: &str = "examples/discovery/bin/discovery.wasm";

/// Skip the test if the WASM binary hasn't been built.
fn load_discovery_wasm() -> Option<Vec<u8>> {
    std::fs::read(DISCOVERY_WASM_PATH).ok()
}

/// Create a Runtime client for testing (no epoch guard, no network).
/// Spawned cells get a Membrane bootstrap (required for system::serve() export).
fn setup_runtime() -> system_capnp::runtime::Client {
    let network_state = NetworkState::new();
    let (swarm_tx, _swarm_rx) = mpsc::channel(16);

    create_runtime_client(
        network_state,
        swarm_tx,
        false,
        None, // no epoch guard (testing, not production)
        None,
        None, // no signing key
        None,
        CachePolicy::Shared,
    )
}

/// Load the discovery WASM via runtime.load(), spawn in cell mode, and
/// get its bootstrap Greeter cap.
async fn spawn_greeter_cell(
    runtime: &system_capnp::runtime::Client,
    wasm: &[u8],
) -> greeter_capnp::greeter::Client {
    // runtime.load(wasm) → Executor
    let mut load_req = runtime.load_request();
    load_req.get().set_wasm(wasm);
    let load_resp = load_req.send().promise.await.expect("runtime.load failed");
    let executor = load_resp.get().unwrap().get_executor().unwrap();

    // executor.spawn(args, env) → Process
    let mut req = executor.spawn_request();
    {
        // No args = cell mode (default). No WW_CELL envvar needed.
        let mut env = req.get().init_env(1);
        env.set(0, "WW_PEER_ID=deadbeefcafebabe");
    }
    let resp = req.send().promise.await.expect("executor.spawn failed");
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
            let runtime = setup_runtime();
            let greeter = spawn_greeter_cell(&runtime, &wasm).await;

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
            let runtime = setup_runtime();
            let greeter = spawn_greeter_cell(&runtime, &wasm).await;

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
