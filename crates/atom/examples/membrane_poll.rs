//! Example: Indexer → Finalizer → adopted Epoch → Membrane → graft → statusPoller.pollStatus.
//!
//! Demonstrates the real authority model: run the indexer and finalizer until the first
//! finalized event, then construct the membrane from that epoch, graft, and poll. When a
//! new epoch is adopted (user triggers setHead in another terminal), the same poller
//! fails with an RPC error; re-graft to obtain a new session and poll Ok again.
//!
//! Usage:
//!
//!   cargo run -p atom --example membrane_poll -- --ws-url <WS_URL> --http-url <HTTP_URL> --contract <ATOM_ADDRESS>
//!
//! Options:
//!   --depth <K>   Confirmation depth (blocks after event before finalized). Default: 2.

use capnp_rpc::new_client;
use atom::stem_capnp;
use atom::{current_block_number, FinalizerBuilder, IndexerConfig, AtomIndexer, Epoch};
use atom::{FinalizedEvent, membrane_client};
use std::sync::Arc;
use tokio::sync::watch;

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

fn finalized_to_epoch(e: &FinalizedEvent) -> Epoch {
    Epoch {
        seq: e.seq,
        head: e.cid.clone(),
        adopted_block: e.block_number,
    }
}

/// Stub Signer for graft: returns empty signature (example only).
struct StubSigner;

#[allow(refining_impl_trait)]
impl stem_capnp::signer::Server for StubSigner {
    fn sign(
        self: capnp::capability::Rc<Self>,
        _: stem_capnp::signer::SignParams,
        mut results: stem_capnp::signer::SignResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        results.get().init_sig(0);
        capnp::capability::Promise::ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();
    let mut ws_url = String::new();
    let mut http_url = String::new();
    let mut contract = String::new();
    let mut depth: u64 = 2;
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
            "--depth" => {
                i += 1;
                if let Some(s) = args.get(i) {
                    depth = s.parse().unwrap_or(2);
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "Usage: membrane_poll --ws-url <WS_URL> --http-url <HTTP_URL> --contract <ATOM_ADDRESS> [--depth K]\n\
                     Runs indexer → finalizer → membrane → graft → pollStatus (live-only from current block). Use small --depth (1–2) on a dev chain.\n\
                     After first epoch, run the printed cast send in another terminal to trigger staleness + re-graft."
                );
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    if ws_url.is_empty() || http_url.is_empty() || contract.is_empty() {
        eprintln!("Usage: membrane_poll --ws-url <WS_URL> --http-url <HTTP_URL> --contract <ATOM_ADDRESS> [--depth K]");
        std::process::exit(1);
    }
    let contract_address = match parse_contract_address(&contract) {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    let mut finalizer = FinalizerBuilder::new()
        .http_url(&http_url)
        .contract_address(contract_address)
        .confirmation_depth(depth)
        .build()?;

    let signer_client: stem_capnp::signer::Client = new_client(StubSigner);
    let contract_display = format!("0x{}", hex::encode(contract_address));

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let start_block = current_block_number(&http_url)
            .await
            .map_err(|e| capnp::Error::failed(format!("current_block_number: {}", e)))?;
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

        let mut epoch_tx: Option<watch::Sender<Epoch>> = None;
        let mut membrane: Option<stem_capnp::membrane::Client<capnp::any_pointer::Owned>> = None;
        let mut poller: Option<stem_capnp::status_poller::Client> = None;
        let mut first_issued_seq: Option<u64> = None;
        let mut last_adopted_seq: Option<u64> = None;
        let mut printed_cast_command = false;
        let mut demo_done = false;

        while !demo_done {
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
                        let epoch = finalized_to_epoch(&e);
                        let current_seq = epoch.seq;

                        if let Some(tx) = &epoch_tx {
                            // New epoch adopted: send it, print epoch_advanced, poll same poller -> RPC error, re-graft -> Ok.
                            tx.send(epoch.clone()).ok();
                            let old_seq = last_adopted_seq.unwrap_or(0);
                            println!("epoch_advanced old_seq={} new_seq={}", old_seq, current_seq);

                            let issued_seq = first_issued_seq.unwrap_or(0);
                            let p = poller.as_ref().unwrap().poll_status_request();
                            match p.send().promise.await {
                                Ok(_) => panic!("poll_status should fail with RPC error after epoch advance"),
                                Err(e) => {
                                    println!("issued_seq={} current_seq={} poll_error={}", issued_seq, current_seq, e);
                                    assert!(e.to_string().contains("staleEpoch"));
                                }
                            }

                            let bootstrap = membrane.as_ref().unwrap();
                            let mut graft_req2 = bootstrap.graft_request();
                            graft_req2.get().set_signer(signer_client.clone());
                            let graft_rpc2 = graft_req2.send().promise.await?;
                            let graft_res2 = graft_rpc2.get()?;
                            let session2 = graft_res2.get_session()?;
                            let new_issued_seq = session2.get_issued_epoch()?.get_seq();
                            first_issued_seq = Some(new_issued_seq);
                            poller = Some(session2.get_status_poller()?);

                            let p2 = poller.as_ref().unwrap().poll_status_request();
                            let r2 = p2.send().promise.await?;
                            let status2 = r2.get()?.get_status()?;
                            let status_str = match status2 {
                                stem_capnp::Status::Ok => "Ok",
                                stem_capnp::Status::Unauthorized => "Unauthorized",
                                stem_capnp::Status::InternalError => "InternalError",
                            };
                            println!("issued_seq={} current_seq={} status={}", new_issued_seq, current_seq, status_str);
                            assert_eq!(status2, stem_capnp::Status::Ok);

                            last_adopted_seq = Some(current_seq);
                            demo_done = true;
                        } else {
                            // First finalized event: create channel, then membrane, graft, poll.
                            let (tx, rx) = watch::channel(epoch.clone());
                            epoch_tx = Some(tx);
                            let bootstrap = membrane_client(rx);
                            membrane = Some(bootstrap.clone());

                            let mut graft_req = bootstrap.graft_request();
                            graft_req.get().set_signer(signer_client.clone());
                            let graft_rpc = graft_req.send().promise.await?;
                            let graft_res = graft_rpc.get()?;
                            let session = graft_res.get_session()?;
                            let issued_seq = session.get_issued_epoch()?.get_seq();
                            first_issued_seq = Some(issued_seq);
                            poller = Some(session.get_status_poller()?);

                            println!(
                                "adopted epoch seq={} adopted_block={} head_len={}",
                                current_seq,
                                epoch.adopted_block,
                                epoch.head.len()
                            );

                            let p = poller.as_ref().unwrap().poll_status_request();
                            let r = p.send().promise.await?;
                            let status = r.get()?.get_status()?;
                            let status_str = match status {
                                stem_capnp::Status::Ok => "Ok",
                                stem_capnp::Status::Unauthorized => "Unauthorized",
                                stem_capnp::Status::InternalError => "InternalError",
                            };
                            println!("issued_seq={} current_seq={} status={}", issued_seq, current_seq, status_str);
                            assert_eq!(status, stem_capnp::Status::Ok);

                            last_adopted_seq = Some(current_seq);

                            if !printed_cast_command {
                                eprintln!(
                                    "Trigger a second head update by running in another terminal:"
                                );
                                eprintln!(
                                    "  cast send {} \"setHead(bytes)\" 0x697066732f2f7365636f6e64 --rpc-url {} --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                                    contract_display,
                                    http_url
                                );
                                printed_cast_command = true;
                            }
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        Ok::<(), capnp::Error>(())
    })?;
    Ok(())
}
