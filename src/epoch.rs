//! Epoch pipeline: chain indexer → finalizer → IPFS pinning → epoch broadcast.
//!
//! Watches an Atom contract for `HeadUpdated` events, waits for finality
//! (configurable confirmation depth), manages IPFS pinsets, and advances
//! the epoch channel so guest sessions auto-invalidate.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::watch;
use tracing::{info, warn};

use atom::{AtomIndexer, FinalizedEvent, FinalizerBuilder, IndexerConfig};
use membrane::Epoch;

use crate::image::cid_bytes_to_ipfs_path;
use crate::ipfs;

/// Run the epoch pipeline until the indexer exits or an unrecoverable error occurs.
///
/// 1. Subscribes to `HeadUpdated` events via `AtomIndexer`.
/// 2. Feeds them to a `Finalizer` with the given confirmation depth.
/// 3. On finalization: pins the new CID, unpins the old, sends the new `Epoch`.
pub async fn run_epoch_pipeline(
    config: IndexerConfig,
    epoch_tx: watch::Sender<Epoch>,
    confirmation_depth: u64,
    ipfs_client: ipfs::HttpClient,
) -> Result<()> {
    let indexer = Arc::new(AtomIndexer::new(config.clone()));
    let mut events = indexer.subscribe();

    // Spawn the indexer (runs its own reconnection loop).
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
                    if let Err(e) =
                        handle_finalized(&fe, &epoch_tx, &ipfs_client, &mut prev_ipfs_path).await
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

/// Process a single finalized event: pin new CID, unpin old, advance epoch.
async fn handle_finalized(
    fe: &FinalizedEvent,
    epoch_tx: &watch::Sender<Epoch>,
    ipfs_client: &ipfs::HttpClient,
    prev_ipfs_path: &mut Option<String>,
) -> Result<()> {
    let ipfs_path =
        cid_bytes_to_ipfs_path(&fe.cid).context("Failed to convert finalized CID to IPFS path")?;

    // Pin the new head.
    if let Err(e) = ipfs_client.pin_add(&ipfs_path).await {
        warn!(path = %ipfs_path, "Failed to pin new head (continuing): {e}");
    } else {
        info!(seq = fe.seq, path = %ipfs_path, "Pinned new head");
    }

    // Unpin the previous head.
    if let Some(prev) = prev_ipfs_path.take() {
        if let Err(e) = ipfs_client.pin_rm(&prev).await {
            warn!(path = %prev, "Failed to unpin old head (continuing): {e}");
        } else {
            info!(path = %prev, "Unpinned old head");
        }
    }

    *prev_ipfs_path = Some(ipfs_path);

    let new_epoch = Epoch {
        seq: fe.seq,
        head: fe.cid.clone(),
        adopted_block: fe.block_number,
    };

    info!(
        seq = new_epoch.seq,
        block = new_epoch.adopted_block,
        "Advancing epoch"
    );

    epoch_tx.send(new_epoch).ok();
    Ok(())
}
