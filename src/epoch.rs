//! Epoch pipeline: chain indexer → finalizer → IPFS pinning → epoch broadcast.
//!
//! Watches an Atom contract for `HeadUpdated` events, waits for finality
//! (configurable confirmation depth), manages IPFS pinsets, and advances
//! the epoch channel so guest sessions auto-invalidate.

use std::sync::Arc;
use std::time::Duration;

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
///
/// `drain_duration` controls graceful shutdown: when non-zero, capabilities have
/// this long to finish in-flight work before the epoch advances and they die.
/// During the drain window the FS already serves new content (CidTree was swapped),
/// but capabilities still reference the old epoch.
pub async fn run_epoch_pipeline(
    config: IndexerConfig,
    epoch_tx: watch::Sender<Epoch>,
    confirmation_depth: u64,
    ipfs_client: ipfs::HttpClient,
    cid_tree: Option<Arc<crate::vfs::CidTree>>,
    drain_duration: Duration,
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
                    if let Err(e) = handle_finalized(
                        &fe,
                        &epoch_tx,
                        &ipfs_client,
                        &mut prev_ipfs_path,
                        cid_tree.as_ref(),
                        drain_duration,
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

/// Process a single finalized event: pin new CID, swap CidTree, unpin old, advance epoch.
///
/// When a CidTree is present, the ordering follows the two-layer revocation model:
/// 1. Pin new CID
/// 2. Pre-warm CidTree directory cache for new root
/// 3. Swap CidTree root (FS sees new content)
/// 4. Unpin old CID
/// 5. Drain — SIGTERM window for in-flight operations
/// 6. Broadcast epoch — SIGKILL, capabilities die, guests re-negotiate
async fn handle_finalized(
    fe: &FinalizedEvent,
    epoch_tx: &watch::Sender<Epoch>,
    ipfs_client: &ipfs::HttpClient,
    prev_ipfs_path: &mut Option<String>,
    cid_tree: Option<&Arc<crate::vfs::CidTree>>,
    drain_duration: Duration,
) -> Result<()> {
    let ipfs_path =
        cid_bytes_to_ipfs_path(&fe.cid).context("Failed to convert finalized CID to IPFS path")?;

    // Extract CID string from /ipfs/<cid>
    let cid_str = ipfs_path
        .strip_prefix("/ipfs/")
        .unwrap_or(&ipfs_path)
        .to_string();

    // 1. Pin the new head.
    if let Err(e) = ipfs_client.pin_add(&ipfs_path).await {
        warn!(path = %ipfs_path, "Failed to pin new head (continuing): {e}");
    } else {
        info!(seq = fe.seq, path = %ipfs_path, "Pinned new head");
    }

    // 2-3. Pre-warm and swap CidTree root (FS swap happens-before capability death).
    if let Some(tree) = cid_tree {
        if let Err(e) = tree.pre_warm(&cid_str).await {
            warn!(cid = %cid_str, "CidTree pre-warm failed (continuing): {e}");
        }
        tree.swap_root(cid_str);
        info!(seq = fe.seq, "CidTree root swapped");
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

    // 5. Drain — give in-flight operations time to finish.
    if !drain_duration.is_zero() {
        info!(
            seq = fe.seq,
            drain_ms = drain_duration.as_millis() as u64,
            "Draining epoch — capabilities have {}s to finish",
            drain_duration.as_secs_f32()
        );
        tokio::time::sleep(drain_duration).await;
    }

    // 6. Broadcast epoch (capabilities die, guests re-negotiate).
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
