//! Example: run the indexer and finalizer, printing only post-finality events.
//!
//! Builds the indexer first, then drives a finalizer in the main loop. Prints one-line JSON
//! per finalized event (confirmation-depth strategy; works with local chains e.g. Anvil).
//!
//! Usage:
//!
//!   cargo run -p atom --example finalizer -- --ws-url <WS_URL> --http-url <HTTP_URL> --contract <ATOM_ADDRESS>
//!
//! Options:
//!   --depth <K>   Confirmation depth (number of blocks after event before considering finalized). Default: 6.
//!   --cursor <path>  Path to file containing start block (one line, decimal). If missing or invalid, start from 0.

use atom::{FinalizerBuilder, IndexerConfig, AtomIndexer};
use std::io::BufRead;
use std::sync::Arc;

fn parse_contract_address(s: &str) -> Result<[u8; 20], String> {
    let addr_hex = s.strip_prefix("0x").unwrap_or(s);
    let addr_bytes = hex::decode(addr_hex).map_err(|e| e.to_string())?;
    if addr_bytes.len() != 20 {
        return Err("contract must be 20 bytes (40 hex chars)".into());
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&addr_bytes);
    Ok(out)
}

fn read_start_block_from_file(path: &str) -> u64 {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let mut line = String::new();
    let mut reader = std::io::BufReader::new(file);
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return 0;
    }
    line.trim().parse().unwrap_or(0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();
    let mut ws_url = String::new();
    let mut http_url = String::new();
    let mut contract = String::new();
    let mut cursor_path = String::new();
    let mut depth: u64 = 6;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--ws-url" => {
                i += 1;
                ws_url = args.get(i).cloned().unwrap_or_default();
            }
            "--http-url" => {
                i += 1;
                http_url = args.get(i).cloned().unwrap_or_default();
            }
            "--contract" => {
                i += 1;
                contract = args.get(i).cloned().unwrap_or_default();
            }
            "--cursor" => {
                i += 1;
                cursor_path = args.get(i).cloned().unwrap_or_default();
            }
            "--depth" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    depth = s.parse().unwrap_or(6);
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "Usage: finalizer --ws-url <WS_URL> --http-url <HTTP_URL> --contract <ATOM_ADDRESS> [--depth K] [--cursor <path>]\n\
                     Prints one-line JSON per finalized HeadUpdated event (confirmation-depth strategy).\n\
                     --depth K  Confirmation depth (blocks after event before finalized). Default: 6.\n\
                     --cursor   Path to file with start block (one line, decimal). Optional.\n\
                     Works with local chains (e.g. Anvil)."
                );
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    if ws_url.is_empty() || http_url.is_empty() || contract.is_empty() {
        eprintln!("Usage: finalizer --ws-url <WS_URL> --http-url <HTTP_URL> --contract <ATOM_ADDRESS> [--depth K] [--cursor <path>]");
        std::process::exit(1);
    }
    let contract_address = match parse_contract_address(&contract) {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };
    let start_block = if cursor_path.is_empty() {
        0
    } else {
        read_start_block_from_file(&cursor_path)
    };

    let config = IndexerConfig {
        ws_url: ws_url.clone(),
        http_url: http_url.clone(),
        contract_address,
        start_block,
        getlogs_max_range: 1000,
        reconnection: Default::default(),
    };
    let indexer = Arc::new(AtomIndexer::new(config));
    let mut recv = indexer.subscribe();
    let indexer_clone = Arc::clone(&indexer);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _ = indexer_clone.run().await;
        });
    });

    let mut finalizer = FinalizerBuilder::new()
        .http_url(&http_url)
        .contract_address(contract_address)
        .confirmation_depth(depth)
        .build()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        loop {
            tokio::select! {
                Ok(ev) = recv.recv() => {
                    finalizer.feed(ev);
                    let tip = match finalizer.current_tip().await {
                        Ok(t) => t,
                        Err(e) => {
                            tracing::warn!(%e, "current_tip failed");
                            continue;
                        }
                    };
                    let events = match finalizer.drain_eligible(tip).await {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::warn!(%e, "drain_eligible failed");
                            continue;
                        }
                    };
                    for e in events {
                        let json = serde_json::to_string(&e).unwrap();
                        println!("{}", json);
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }
    });
    Ok(())
}
