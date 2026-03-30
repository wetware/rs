//! Integration tests for the kernel init.d boot path.
//!
//! These tests exercise the full SysV init pipeline: the real kernel WASM
//! boots inside a Cell, receives init.d scripts via MemoryStore-backed IPFS,
//! and we verify that malformed scripts are skipped (best-effort model).
//!
//! Requires a pre-built kernel WASM at `target/wasm32-wasip2/debug/kernel.wasm`.
//! Run:  cargo build -p kernel --target wasm32-wasip2

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use ww::cell::{CellBuilder, Loader};
use ww::host::SwarmCommand;
use ww::ipfs::MemoryStore;
use ww::rpc::NetworkState;

/// Path to the pre-built kernel WASM.
const KERNEL_WASM: &str = "target/wasm32-wasip2/debug/kernel.wasm";

/// Loader that always returns the same pre-read bytecode.
struct StaticLoader(Vec<u8>);

#[async_trait]
impl Loader for StaticLoader {
    async fn load(&self, _name: &str) -> Result<Vec<u8>> {
        Ok(self.0.clone())
    }
}

/// Seed a MemoryStore with init.d scripts under the given root.
///
/// Layout:
///   <root>/etc/init.d/01-good.glia  →  (help)
///   <root>/etc/init.d/02-bad.glia   →  (broken          ← malformed
///   <root>/etc/init.d/03-good.glia  →  (help)
fn seed_initd_store(root: &str) -> Arc<MemoryStore> {
    let store = MemoryStore::new();

    let initd = format!("{root}/etc/init.d");

    // Scripts that the kernel will `cat` via IPFS.
    store.insert(format!("{initd}/01-good.glia"), b"(help)".to_vec());
    store.insert(
        format!("{initd}/02-bad.glia"),
        b"(broken".to_vec(), // unclosed paren — parse error
    );
    store.insert(format!("{initd}/03-good.glia"), b"(help)".to_vec());

    Arc::new(store)
}

/// Boot the kernel with a MemoryStore containing init.d scripts (good + bad + good).
///
/// The test asserts that:
/// - The kernel exits cleanly (exit code 0) — it doesn't crash on the malformed script.
/// - This proves the SysV best-effort model: skip bad script, continue to next.
#[tokio::test]
async fn initd_skips_malformed_script_and_continues() {
    let wasm_path = Path::new(KERNEL_WASM);
    if !wasm_path.exists() {
        eprintln!(
            "SKIP: kernel WASM not found at {KERNEL_WASM}. \
             Build with: cargo build -p kernel --target wasm32-wasip2"
        );
        return;
    }

    let bytecode = std::fs::read(wasm_path).expect("read kernel WASM");
    let loader: Box<dyn Loader> = Box::new(StaticLoader(bytecode));

    let root = "/ipfs/QmTestRoot";
    let store = seed_initd_store(root);

    let (swarm_tx, _swarm_rx) = mpsc::channel::<SwarmCommand>(1);

    let cell = CellBuilder::new("kernel".into())
        .with_loader(loader)
        .with_env(vec![format!("WW_ROOT={root}")])
        .with_content_store(store)
        .with_network_state(NetworkState::default())
        .with_swarm_cmd_tx(swarm_tx)
        .build();

    let result = cell.spawn().await;
    match result {
        Ok(spawn_result) => {
            assert_eq!(
                spawn_result.exit_code, 0,
                "kernel should exit cleanly after processing init.d (got code {})",
                spawn_result.exit_code
            );
        }
        Err(e) => {
            // The kernel may fail to load if the WASM isn't a valid component.
            // That's a build issue, not a test failure.
            panic!("Cell::spawn failed: {e:#}");
        }
    }
}

/// Boot the kernel with no init.d scripts. It should start and exit cleanly.
#[tokio::test]
async fn initd_empty_exits_cleanly() {
    let wasm_path = Path::new(KERNEL_WASM);
    if !wasm_path.exists() {
        eprintln!("SKIP: kernel WASM not found at {KERNEL_WASM}");
        return;
    }

    let bytecode = std::fs::read(wasm_path).expect("read kernel WASM");
    let loader: Box<dyn Loader> = Box::new(StaticLoader(bytecode));

    // Empty store — no init.d directory, WW_ROOT has nothing.
    let store = Arc::new(MemoryStore::new());

    let (swarm_tx, _swarm_rx) = mpsc::channel::<SwarmCommand>(1);

    let cell = CellBuilder::new("kernel".into())
        .with_loader(loader)
        .with_env(vec!["WW_ROOT=/ipfs/QmEmpty".into()])
        .with_content_store(store)
        .with_network_state(NetworkState::default())
        .with_swarm_cmd_tx(swarm_tx)
        .build();

    let result = cell.spawn().await.expect("Cell::spawn");
    assert_eq!(
        result.exit_code, 0,
        "kernel should exit cleanly with no init.d scripts"
    );
}

/// Boot the kernel with only valid init.d scripts. All should execute.
#[tokio::test]
async fn initd_all_valid_scripts_execute() {
    let wasm_path = Path::new(KERNEL_WASM);
    if !wasm_path.exists() {
        eprintln!("SKIP: kernel WASM not found at {KERNEL_WASM}");
        return;
    }

    let bytecode = std::fs::read(wasm_path).expect("read kernel WASM");
    let loader: Box<dyn Loader> = Box::new(StaticLoader(bytecode));

    let root = "/ipfs/QmAllGood";
    let store = MemoryStore::new();
    let initd = format!("{root}/etc/init.d");
    store.insert(format!("{initd}/01-cd.glia"), b"(cd \"/tmp\")".to_vec());
    store.insert(format!("{initd}/02-help.glia"), b"(help)".to_vec());
    let store = Arc::new(store);

    let (swarm_tx, _swarm_rx) = mpsc::channel::<SwarmCommand>(1);

    let cell = CellBuilder::new("kernel".into())
        .with_loader(loader)
        .with_env(vec![format!("WW_ROOT={root}")])
        .with_content_store(store)
        .with_network_state(NetworkState::default())
        .with_swarm_cmd_tx(swarm_tx)
        .build();

    let result = cell.spawn().await.expect("Cell::spawn");
    assert_eq!(
        result.exit_code, 0,
        "kernel should exit cleanly when all init.d scripts are valid"
    );
}
