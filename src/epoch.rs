//! Epoch pipeline: source-agnostic pin/swap/broadcast for epoch advances.
//!
//! The pipeline handles the downstream side of any epoch source:
//! 1. Pin new CID on IPFS
//! 2. Pre-warm and swap CidTree root (FS sees new content)
//! 3. Unpin old CID
//! 4. Broadcast epoch (capabilities die, guests re-negotiate)
//!
//! For the Atom-specific indexer+finalizer pipeline (legacy entry point),
//! see `run_atom_pipeline` which wraps `AtomSource` from `crates/stem/`.

use std::sync::Arc;

use anyhow::{Context, Result};
use membrane::Epoch;
use stem::StemEvent;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::image::cid_bytes_to_ipfs_path;
use crate::ipfs;

/// Process a single epoch advance: pin new CID, swap CidTree, unpin old, broadcast.
///
/// This is the shared downstream pipeline that all epoch sources feed into.
/// The ordering follows the two-layer revocation model:
/// 1. Pin new CID
/// 2. Pre-warm CidTree directory cache for new root
/// 3. Swap CidTree root (FS sees new content)
/// 4. Unpin old CID
/// 5. Broadcast epoch (capabilities die, guests re-negotiate)
pub async fn handle_epoch_advance(
    event: &StemEvent,
    epoch_tx: &watch::Sender<Epoch>,
    ipfs_client: &ipfs::HttpClient,
    prev_ipfs_path: &mut Option<String>,
    cid_tree: Option<&Arc<crate::vfs::CidTree>>,
) -> Result<()> {
    let ipfs_path =
        cid_bytes_to_ipfs_path(&event.cid).context("Failed to convert CID to IPFS path")?;

    // Extract CID string from /ipfs/<cid>
    let cid_str = ipfs_path
        .strip_prefix("/ipfs/")
        .unwrap_or(&ipfs_path)
        .to_string();

    // 1. Pin the new head.
    if let Err(e) = ipfs_client.pin_add(&ipfs_path).await {
        warn!(path = %ipfs_path, "Failed to pin new head (continuing): {e}");
    } else {
        info!(seq = event.seq, path = %ipfs_path, "Pinned new head");
    }

    // 2-3. Pre-warm and swap CidTree root (FS swap happens-before capability death).
    if let Some(tree) = cid_tree {
        if let Err(e) = tree.pre_warm(&cid_str).await {
            warn!(cid = %cid_str, "CidTree pre-warm failed (continuing): {e}");
        }
        tree.swap_root(cid_str);
        info!(seq = event.seq, "CidTree root swapped");
    }

    // 4. Unpin the previous head.
    if let Some(prev) = prev_ipfs_path.take() {
        if let Err(e) = ipfs_client.pin_rm(&prev).await {
            warn!(path = %prev, "Failed to unpin old head (continuing): {e}");
        } else {
            info!(path = %prev, "Unpinned old head");
        }
    }

    *prev_ipfs_path = Some(ipfs_path);

    // 5. Broadcast epoch (capabilities die, guests re-negotiate).
    let new_epoch = Epoch {
        seq: event.seq,
        head: event.cid.clone(),
        provenance: event.provenance.clone(),
    };

    info!(
        seq = new_epoch.seq,
        ?new_epoch.provenance,
        "Advancing epoch"
    );

    epoch_tx.send(new_epoch).ok();
    Ok(())
}

// ── Legacy entry point (Atom-specific) ──────────────────────────────
//
// Preserved for backward compatibility with callers that use the old
// `run_epoch_pipeline` signature. Internally delegates to `AtomSource`.

use atom::{AtomIndexer, FinalizerBuilder, IndexerConfig};
use membrane::Provenance;

/// Run the Atom-specific epoch pipeline (legacy entry point).
///
/// Wraps the old AtomIndexer + Finalizer flow with the shared
/// `handle_epoch_advance` downstream. Callers should migrate to
/// `AtomSource::run()` from `crates/stem/` for new code.
pub async fn run_epoch_pipeline(
    config: IndexerConfig,
    epoch_tx: watch::Sender<Epoch>,
    confirmation_depth: u64,
    ipfs_client: ipfs::HttpClient,
    cid_tree: Option<Arc<crate::vfs::CidTree>>,
) -> Result<()> {
    let indexer = Arc::new(AtomIndexer::new(config.clone()));
    let mut events = indexer.subscribe();

    let indexer_handle = {
        let idx = indexer.clone();
        tokio::spawn(async move {
            if let Err(e) = idx.run().await {
                tracing::error!("Atom indexer exited with error: {e}");
            }
        })
    };

    let mut finalizer = FinalizerBuilder::new()
        .http_url(&config.http_url)
        .contract_address(config.contract_address)
        .confirmation_depth(confirmation_depth)
        .build()
        .context("Failed to build finalizer")?;

    let mut prev_ipfs_path: Option<String> = None;

    loop {
        match events.recv().await {
            Ok(ev) => {
                finalizer.feed(ev);

                let tip = match finalizer.current_tip().await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("Failed to fetch chain tip: {e}");
                        continue;
                    }
                };

                let finalized = match finalizer.drain_eligible(tip).await {
                    Ok(f) => f,
                    Err(e) => {
                        warn!("Finalizer drain error: {e}");
                        continue;
                    }
                };

                for fe in finalized {
                    let stem_event = StemEvent {
                        seq: fe.seq,
                        cid: fe.cid.clone(),
                        provenance: Provenance::Block(fe.block_number),
                    };
                    if let Err(e) = handle_epoch_advance(
                        &stem_event,
                        &epoch_tx,
                        &ipfs_client,
                        &mut prev_ipfs_path,
                        cid_tree.as_ref(),
                    )
                    .await
                    {
                        warn!(seq = fe.seq, "Failed to handle finalized event: {e}");
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                warn!(
                    skipped = n,
                    "Epoch pipeline lagged; some events were dropped"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                info!("Indexer channel closed; epoch pipeline shutting down");
                break;
            }
        }
    }

    indexer_handle.abort();
    Ok(())
}
