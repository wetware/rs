//! Finalizer: reorg-safe finalization of observed HeadUpdated events.
//!
//! Consumes observed [HeadUpdatedObserved] from the indexer and outputs only events that are
//! eligible per the configured [Strategy] and pass the canonical cross-check (`Atom.head()`).
//! Dedup key is `(tx_hash, log_index)` (globally unique per log; stable across reconnects/backfill).
//! Configure via [Strategy]; use [ConfirmationDepth] for depth-K finalization. See the
//! `finalizer` example for a full pipeline (indexer → finalizer → JSON output).

use crate::abi::{decode_head_return, HeadUpdatedObserved, HEAD_SELECTOR};
use serde::Serialize;
use std::collections::HashSet;
use thiserror::Error;

/// Defines when an observed event is eligible for finalization given the current chain tip.
pub trait Strategy: Send + Sync {
    /// Returns true if the event has enough confirmations (or otherwise meets the strategy).
    fn is_eligible(&self, ev: &HeadUpdatedObserved, tip: u64) -> bool;
}

/// Confirmation-depth strategy: eligible when `tip >= event.block_number + K`.
#[derive(Debug, Clone)]
pub struct ConfirmationDepth(pub u64);

impl Strategy for ConfirmationDepth {
    fn is_eligible(&self, ev: &HeadUpdatedObserved, tip: u64) -> bool {
        tip >= ev.block_number.saturating_add(self.0)
    }
}

/// One finalized event, ready for JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct FinalizedEvent {
    pub seq: u64,
    /// Raw head bytes from the event (used to build Epoch.head).
    pub cid: Vec<u8>,
    #[serde(rename = "cid_hash")]
    pub cid_hash_hex: String,
    /// Block that satisfied eligibility; use as Epoch.adopted_block.
    pub block_number: u64,
    #[serde(rename = "tx_hash")]
    pub tx_hash_hex: String,
    pub log_index: u64,
    pub writer: String,
}

impl FinalizedEvent {
    fn from_observed(ev: &HeadUpdatedObserved) -> Self {
        Self {
            seq: ev.seq,
            cid: ev.cid.clone(),
            cid_hash_hex: hex::encode(ev.cid_hash),
            block_number: ev.block_number,
            tx_hash_hex: hex::encode(ev.tx_hash),
            log_index: ev.log_index,
            writer: hex::encode(ev.writer),
        }
    }
}

#[derive(Debug, Error)]
pub enum FinalizerError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("decode error: {0}")]
    Decode(String),
}

fn dedup_key(ev: &HeadUpdatedObserved) -> String {
    format!("{}:{}", hex::encode(ev.tx_hash), ev.log_index)
}

async fn http_json_rpc(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: serde_json::Value,
    id: u64,
) -> Result<serde_json::Value, FinalizerError> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    let resp = client.post(url).json(&body).send().await?;
    let json: serde_json::Value = resp.json().await?;
    if let Some(err) = json.get("error") {
        return Err(FinalizerError::Rpc(err.to_string()));
    }
    let result = json
        .get("result")
        .cloned()
        .ok_or_else(|| FinalizerError::Decode("Missing result".into()))?;
    Ok(result)
}

async fn eth_block_number(client: &reqwest::Client, http_url: &str) -> Result<u64, FinalizerError> {
    let result = http_json_rpc(client, http_url, "eth_blockNumber", serde_json::json!([]), 1).await?;
    let s = result
        .as_str()
        .ok_or_else(|| FinalizerError::Decode("blockNumber not string".into()))?;
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|e| FinalizerError::Decode(e.to_string()))
}

async fn eth_call(
    client: &reqwest::Client,
    http_url: &str,
    to: &[u8; 20],
    calldata: &[u8],
) -> Result<Vec<u8>, FinalizerError> {
    let params = serde_json::json!([{
        "to": format!("0x{}", hex::encode(to)),
        "data": format!("0x{}", hex::encode(calldata)),
    }, "latest"]);
    let result = http_json_rpc(client, http_url, "eth_call", params, 3).await?;
    let s = result
        .as_str()
        .ok_or_else(|| FinalizerError::Decode("eth_call result not string".into()))?;
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s))
        .map_err(|e| FinalizerError::Decode(e.to_string()))?;
    Ok(bytes)
}

/// Builder for the finalizer.
pub struct FinalizerBuilder {
    strategy: Option<Box<dyn Strategy + Send>>,
    http_url: Option<String>,
    contract_address: Option<[u8; 20]>,
}

impl FinalizerBuilder {
    pub fn new() -> Self {
        Self {
            strategy: None,
            http_url: None,
            contract_address: None,
        }
    }

    /// Set the eligibility strategy (stored as `Box<dyn Strategy + Send>`).
    pub fn strategy(mut self, s: impl Strategy + 'static) -> Self {
        self.strategy = Some(Box::new(s));
        self
    }

    /// Convenience: equivalent to `.strategy(ConfirmationDepth(k))`.
    pub fn confirmation_depth(mut self, k: u64) -> Self {
        self.strategy = Some(Box::new(ConfirmationDepth(k)));
        self
    }

    pub fn http_url(mut self, url: impl Into<String>) -> Self {
        self.http_url = Some(url.into());
        self
    }

    pub fn contract_address(mut self, addr: [u8; 20]) -> Self {
        self.contract_address = Some(addr);
        self
    }

    pub fn build(self) -> Result<Finalizer, FinalizerError> {
        let strategy = self
            .strategy
            .unwrap_or_else(|| Box::new(ConfirmationDepth(6)));
        let http_url = self
            .http_url
            .ok_or_else(|| FinalizerError::Decode("http_url required".into()))?;
        let contract_address = self
            .contract_address
            .ok_or_else(|| FinalizerError::Decode("contract_address required".into()))?;
        let http_client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| FinalizerError::Decode(e.to_string()))?;
        Ok(Finalizer {
            strategy,
            http_client,
            http_url,
            contract_address,
            pending: Vec::new(),
            emitted: HashSet::new(),
        })
    }
}

impl Default for FinalizerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Finalizer: consumes observed events, outputs only eligible and canonical-finalized events.
pub struct Finalizer {
    strategy: Box<dyn Strategy + Send>,
    http_client: reqwest::Client,
    http_url: String,
    contract_address: [u8; 20],
    pending: Vec<HeadUpdatedObserved>,
    emitted: HashSet<String>,
}

impl Finalizer {
    /// Push an observed event into the pending buffer (sorted by block_number, log_index).
    pub fn feed(&mut self, ev: HeadUpdatedObserved) {
        self.pending.push(ev);
        self.pending
            .sort_by_key(|o| (o.block_number, o.log_index));
    }

    /// Return the current chain tip (latest block number) via JSON-RPC.
    pub async fn current_tip(&self) -> Result<u64, FinalizerError> {
        eth_block_number(&self.http_client, &self.http_url).await
    }

    /// Drain events that are eligible per strategy and pass the canonical cross-check.
    /// Eligibility is checked with `strategy.is_eligible(ev, tip)`; then we call `Atom.head()`
    /// and only emit if (seq, cid) matches the candidate. Dedup by (tx_hash, log_index).
    pub async fn drain_eligible(&mut self, tip: u64) -> Result<Vec<FinalizedEvent>, FinalizerError> {
        // Collect eligible in order (block_number, log_index), then remove them from pending.
        let mut eligible: Vec<HeadUpdatedObserved> = self
            .pending
            .iter()
            .filter(|ev| self.strategy.is_eligible(ev, tip))
            .cloned()
            .collect();
        eligible.sort_by_key(|o| (o.block_number, o.log_index));
        self.pending
            .retain(|ev| !self.strategy.is_eligible(ev, tip));

        let mut out = Vec::new();
        for ev in eligible {
            let key = dedup_key(&ev);
            if self.emitted.contains(&key) {
                continue;
            }
            let head_bytes = eth_call(
                &self.http_client,
                &self.http_url,
                &self.contract_address,
                &HEAD_SELECTOR,
            )
            .await?;
            let head = decode_head_return(&head_bytes)
                .map_err(|e| FinalizerError::Decode(e.to_string()))?;
            if head.seq == ev.seq && head.cid == ev.cid {
                self.emitted.insert(key);
                out.push(FinalizedEvent::from_observed(&ev));
            }
            // If mismatch: already dropped from pending, do not emit (reorg'd or superseded).
        }
        Ok(out)
    }
}
