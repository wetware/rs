//! Indexer configuration.

/// Indexer configuration.
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// WebSocket RPC URL for live log subscription.
    pub ws_url: String,
    /// HTTP RPC URL for backfill (eth_getLogs, eth_blockNumber, eth_call).
    pub http_url: String,
    /// Atom contract address (20 bytes).
    pub contract_address: [u8; 20],
    /// First block to backfill from on startup.
    pub start_block: u64,
    /// Max block range per eth_getLogs request.
    pub getlogs_max_range: u64,
    /// Reconnection backoff (initial and max seconds).
    pub reconnection: ReconnectionConfig,
}

/// Reconnection backoff.
#[derive(Debug, Clone)]
pub struct ReconnectionConfig {
    pub initial_backoff_secs: u64,
    pub max_backoff_secs: u64,
}

impl Default for ReconnectionConfig {
    fn default() -> Self {
        Self {
            initial_backoff_secs: 1,
            max_backoff_secs: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnection_config_default() {
        let c = ReconnectionConfig::default();
        assert_eq!(c.initial_backoff_secs, 1);
        assert_eq!(c.max_backoff_secs, 60);
    }
}
