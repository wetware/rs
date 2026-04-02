//! Integration test for the VatListener serve path with a PriceOracle capability.
//!
//! Proves the full RPC chain: persistent capability → serve-mode VatListener →
//! in-memory duplex → peer bootstrap → query prices. No WASM, no network —
//! the oracle capability is constructed on the host side.
//!
//! This is the host-side counterpart to the oracle guest's unit tests, which
//! test the Cap'n Proto round-trip in isolation. Here we exercise the actual
//! `handle_vat_connection_serve` code path that the VatListener uses.

use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use ww::oracle_capnp;

// ---------------------------------------------------------------------------
// Minimal PriceOracle server (host-side, no WASM)
// ---------------------------------------------------------------------------

struct TestOracle {
    pairs: Vec<(&'static str, i64)>,
}

#[allow(refining_impl_trait)]
impl oracle_capnp::price_oracle::Server for TestOracle {
    fn get_price(
        self: std::rc::Rc<Self>,
        params: oracle_capnp::price_oracle::GetPriceParams,
        mut results: oracle_capnp::price_oracle::GetPriceResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let pair = match params.get() {
            Ok(p) => match p.get_pair() {
                Ok(s) => s.to_str().unwrap_or("").to_string(),
                Err(e) => return capnp::capability::Promise::err(e),
            },
            Err(e) => return capnp::capability::Promise::err(e),
        };
        for (name, price) in &self.pairs {
            if *name == pair {
                let mut r = results.get();
                r.set_price(*price);
                r.set_decimals(9);
                r.set_timestamp(1700000000);
                r.set_confidence(0.95);
                return capnp::capability::Promise::ok(());
            }
        }
        capnp::capability::Promise::err(capnp::Error::failed(format!("unknown pair: {pair}")))
    }

    fn get_pairs(
        self: std::rc::Rc<Self>,
        _params: oracle_capnp::price_oracle::GetPairsParams,
        mut results: oracle_capnp::price_oracle::GetPairsResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let mut list = results.get().init_pairs(self.pairs.len() as u32);
        for (i, (name, _)) in self.pairs.iter().enumerate() {
            list.set(i as u32, name);
        }
        capnp::capability::Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test the serve path: `handle_vat_connection_serve` bootstraps a persistent
/// capability that a peer can query multiple times.
#[tokio::test]
async fn test_serve_path_oracle_query() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let oracle = TestOracle {
                pairs: vec![("ETH/gas", 30_000_000_000), ("BASE/gas", 1_000_000)],
            };
            let oracle_client: oracle_capnp::price_oracle::Client =
                capnp_rpc::new_client(oracle);

            // In-memory duplex: one end for the serve bridge, one for the peer.
            let (peer_stream, serve_stream) = tokio::io::duplex(8 * 1024);

            // Spawn the serve-mode handler (what VatListener does for each connection).
            let serve_handle = tokio::task::spawn_local(async move {
                ww::rpc::vat_listener::handle_vat_connection_serve(
                    oracle_client.client,
                    serve_stream.compat(),
                    "test-oracle-cid",
                )
                .await
            });

            // Peer side: bootstrap the oracle capability.
            let (peer_read, peer_write) = tokio::io::split(peer_stream);
            let peer_network = VatNetwork::new(
                peer_read.compat(),
                peer_write.compat_write(),
                Side::Client,
                Default::default(),
            );
            let mut peer_rpc = RpcSystem::new(Box::new(peer_network), None);
            let remote_oracle: oracle_capnp::price_oracle::Client =
                peer_rpc.bootstrap(Side::Server);
            let rpc_handle = tokio::task::spawn_local(async move {
                let _ = peer_rpc.await;
            });

            // Query 1: getPairs
            let pairs_resp = remote_oracle
                .get_pairs_request()
                .send()
                .promise
                .await
                .expect("getPairs failed");
            let pairs = pairs_resp.get().unwrap().get_pairs().unwrap();
            assert_eq!(pairs.len(), 2);

            // Query 2: getPrice for ETH/gas
            let mut req = remote_oracle.get_price_request();
            req.get().set_pair("ETH/gas");
            let resp = req.send().promise.await.expect("getPrice failed");
            let r = resp.get().unwrap();
            assert_eq!(r.get_price(), 30_000_000_000);
            assert_eq!(r.get_decimals(), 9);
            assert!((r.get_confidence() - 0.95).abs() < 0.001);

            // Query 3: getPrice for BASE/gas (second pair)
            let mut req = remote_oracle.get_price_request();
            req.get().set_pair("BASE/gas");
            let resp = req.send().promise.await.expect("getPrice BASE failed");
            assert_eq!(resp.get().unwrap().get_price(), 1_000_000);

            // Query 4: unknown pair returns error
            let mut req = remote_oracle.get_price_request();
            req.get().set_pair("DOGE/usd");
            let resp = req.send().promise.await;
            assert!(resp.is_err(), "unknown pair should error");

            // Disconnect peer — serve handler should return cleanly.
            drop(remote_oracle);
            rpc_handle.abort();

            let result = tokio::time::timeout(std::time::Duration::from_secs(5), serve_handle)
                .await
                .expect("serve handler did not exit after peer disconnect")
                .expect("serve task panicked");
            assert!(result.is_ok(), "serve handler should return Ok");
        })
        .await;
}

/// Multiple peers can connect sequentially to the same serve capability.
/// Each gets independent RPC and full query access.
#[tokio::test]
async fn test_serve_path_sequential_peers() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let oracle = TestOracle {
                pairs: vec![("ETH/gas", 42_000_000_000)],
            };
            let oracle_client: oracle_capnp::price_oracle::Client =
                capnp_rpc::new_client(oracle);

            for i in 0..3 {
                let (peer_stream, serve_stream) = tokio::io::duplex(8 * 1024);

                let cap = oracle_client.client.clone();
                let serve_handle = tokio::task::spawn_local(async move {
                    ww::rpc::vat_listener::handle_vat_connection_serve(
                        cap,
                        serve_stream.compat(),
                        "test-oracle-cid",
                    )
                    .await
                });

                let (peer_read, peer_write) = tokio::io::split(peer_stream);
                let peer_network = VatNetwork::new(
                    peer_read.compat(),
                    peer_write.compat_write(),
                    Side::Client,
                    Default::default(),
                );
                let mut peer_rpc = RpcSystem::new(Box::new(peer_network), None);
                let remote: oracle_capnp::price_oracle::Client =
                    peer_rpc.bootstrap(Side::Server);
                let rpc_handle = tokio::task::spawn_local(async move {
                    let _ = peer_rpc.await;
                });

                let mut req = remote.get_price_request();
                req.get().set_pair("ETH/gas");
                let resp = req.send().promise.await.unwrap();
                assert_eq!(
                    resp.get().unwrap().get_price(),
                    42_000_000_000,
                    "peer {i} should see correct price"
                );

                drop(remote);
                rpc_handle.abort();
                let _ = serve_handle.await;
            }
        })
        .await;
}
