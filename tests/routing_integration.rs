//! Integration tests for content routing (Kubo HTTP backend).
//!
//! Tests provide/findProviders via `/api/v0/routing/*` endpoints.
//! Data transfer (add/cat) is tested in `ipfs_integration.rs`.
//!
//! Requires a running IPFS (Kubo) node. The API endpoint is read from the
//! `IPFS_API_URL` environment variable, defaulting to `http://127.0.0.1:5001`.
//!
//! Tests skip gracefully when the node is unreachable.

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the IPFS HTTP API base URL from env or default.
fn ipfs_api_url() -> String {
    std::env::var("IPFS_API_URL").unwrap_or_else(|_| "http://127.0.0.1:5001".to_string())
}

/// Non-proxy reqwest client (same pattern as ipfs_integration helpers).
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("reqwest client")
}

/// Check whether the IPFS API is reachable. Returns false (skip) on failure.
async fn ipfs_available() -> bool {
    let url = format!("{}/api/v0/id", ipfs_api_url());
    http_client().post(&url).send().await.is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Provide a CID and then query findProviders.
///
/// On a single-node Kubo instance (no DHT peers), `provide` succeeds but
/// `findProviders` returns zero Type-4 (provider) records because there is
/// no DHT network to query.  We therefore assert that both calls complete
/// without error, and only validate provider contents when results are
/// returned (multi-node / CI with a DHT cluster).
#[tokio::test]
async fn test_routing_provide_and_find_providers() {
    if !ipfs_available().await {
        eprintln!("skipping test_routing_provide_and_find_providers: IPFS node not reachable");
        return;
    }

    let client = ww::ipfs::HttpClient::new(ipfs_api_url());

    // Add data via UnixFS so we have a valid CID to provide.
    let payload = b"routing provide/findProviders integration test";
    let cid = client
        .add_bytes(payload)
        .await
        .expect("add_bytes should succeed");

    // Announce ourselves as a provider for this CID.
    client
        .routing_provide(&cid)
        .await
        .expect("routing_provide should succeed");

    // Find providers.  On a single-node setup this may return an empty list
    // (no DHT peers to report providers).  On a multi-node setup we expect
    // at least one provider.
    let providers = client
        .routing_find_providers(&cid, 5)
        .await
        .expect("routing_find_providers should succeed");

    // If we did get results, validate their structure.
    for provider in &providers {
        assert!(
            !provider.id.is_empty(),
            "provider peer ID should not be empty"
        );
    }

    eprintln!(
        "test_routing_provide_and_find_providers: found {} providers for CID {}",
        providers.len(),
        cid
    );
}
