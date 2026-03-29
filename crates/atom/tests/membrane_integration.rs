//! Integration test: Epoch → Terminal → Membrane → graft → executor.echo (epoch-guarded).
//! All local: servers and clients are in-process (capnp-rpc local dispatch).
//!
//! Terminal = authentication gate (challenge-response).
//! Membrane = capability provisioning (ocap: having the reference IS authorization).

mod common;

use atom::stem_capnp;
use atom::{AtomIndexer, Epoch, IndexerConfig, MembraneServer, TerminalServer};
use auth::SigningDomain;
use capnp_rpc::new_client;
use common::{deploy_atom, set_head, spawn_anvil, FullStubSessionBuilder, StubSessionBuilder};
use ed25519_dalek::SigningKey;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;

/// Signer that produces libp2p SignedEnvelopes for Terminal challenge-response.
struct TestSigner {
    keypair: libp2p_identity::Keypair,
}

impl TestSigner {
    fn from_ed25519(sk: &SigningKey) -> Self {
        let ed_kp = libp2p_identity::ed25519::Keypair::try_from_bytes(&mut sk.to_keypair_bytes())
            .expect("valid key");
        Self {
            keypair: ed_kp.into(),
        }
    }
}

#[allow(refining_impl_trait)]
impl stem_capnp::signer::Server for TestSigner {
    fn sign(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::signer::SignParams,
        mut results: stem_capnp::signer::SignResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let nonce = capnp_rpc::pry!(params.get()).get_nonce();
        let domain = SigningDomain::terminal_membrane();

        let envelope = capnp_rpc::pry!(libp2p_core::SignedEnvelope::new(
            &self.keypair,
            domain.as_str().to_string(),
            domain.payload_type().to_vec(),
            nonce.to_be_bytes().to_vec(),
        )
        .map_err(|e| capnp::Error::failed(format!("signing failed: {e}"))));

        results.get().set_sig(&envelope.into_protobuf_encoding());
        capnp::capability::Promise::ok(())
    }
}

fn observed_to_epoch(ev: &atom::HeadUpdatedObserved) -> Epoch {
    Epoch {
        seq: ev.seq,
        head: ev.cid.clone(),
        adopted_block: ev.block_number,
    }
}

/// Helper: create a Membrane client (no auth — pure ocap).
fn stub_membrane(rx: watch::Receiver<Epoch>) -> stem_capnp::membrane::Client {
    new_client(MembraneServer::new(rx, StubSessionBuilder))
}

/// Helper: wrap a Membrane client in Terminal (challenge-response auth gate).
fn terminal_membrane(
    rx: watch::Receiver<Epoch>,
    vk: ed25519_dalek::VerifyingKey,
) -> stem_capnp::terminal::Client<stem_capnp::membrane::Owned> {
    let membrane = stub_membrane(rx);
    new_client(TerminalServer::<stem_capnp::membrane::Owned>::new(
        vk,
        membrane,
        SigningDomain::terminal_membrane(),
    ))
}

#[tokio::test]
async fn test_membrane_graft_echo_against_anvil() {
    if !common::foundry_available() {
        eprintln!("skipping test_membrane_graft_echo_against_anvil: anvil/forge/cast not in PATH");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("atom=debug".parse().unwrap()))
        .with_test_writer()
        .try_init();

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    let (mut anvil_process, rpc_url) = spawn_anvil().await.expect("spawn anvil");
    let contract_addr = deploy_atom(repo_root, &rpc_url).expect("deploy Atom");
    let addr_bytes =
        hex::decode(contract_addr.strip_prefix("0x").unwrap_or(&contract_addr)).expect("hex");
    let mut contract_address = [0u8; 20];
    contract_address.copy_from_slice(&addr_bytes);

    set_head(
        repo_root,
        &rpc_url,
        &contract_addr,
        "setHead(bytes)",
        "0x697066732f2f6669727374",
        None,
    )
    .expect("setHead 1");

    let ws_url = rpc_url
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let config = IndexerConfig {
        ws_url: ws_url.clone(),
        http_url: rpc_url.clone(),
        contract_address,
        start_block: 0,
        getlogs_max_range: 1000,
        reconnection: Default::default(),
    };
    let indexer = Arc::new(AtomIndexer::new(config));
    let mut recv = indexer.subscribe();
    let indexer_clone = Arc::clone(&indexer);
    let indexer_task = tokio::spawn(async move {
        let _ = indexer_clone.run().await;
    });

    let first_ev = timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(ev) = recv.recv().await {
                return ev;
            }
        }
    })
    .await
    .expect("timeout waiting for first event");

    indexer_task.abort();
    let _ = anvil_process.kill();

    let epoch1 = observed_to_epoch(&first_ev);
    let epoch2 = Epoch {
        seq: first_ev.seq + 1,
        head: b"next_head".to_vec(),
        adopted_block: first_ev.block_number + 1,
    };

    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let vk = sk.verifying_key();

    let (tx, rx) = watch::channel(epoch1.clone());
    let terminal = terminal_membrane(rx, vk);
    let signer_client: stem_capnp::signer::Client = new_client(TestSigner::from_ed25519(&sk));

    // Login via Terminal → get Membrane → graft → echo → Ok
    let mut login_req = terminal.login_request();
    login_req.get().set_signer(signer_client);
    let login_resp = login_req.send().promise.await.expect("login RPC");
    let membrane = login_resp
        .get()
        .expect("login results")
        .get_session()
        .expect("session");

    let graft_rpc_response = membrane
        .graft_request()
        .send()
        .promise
        .await
        .expect("graft RPC");
    let graft_response = graft_rpc_response.get().expect("graft results");
    let executor = graft_response.get_executor().expect("executor");

    let mut echo_req = executor.echo_request();
    echo_req.get().set_message("hello");
    let echo_resp = echo_req.send().promise.await.expect("echo RPC");
    let response = echo_resp
        .get()
        .expect("echo results")
        .get_response()
        .expect("response");
    assert_eq!(response.to_str().unwrap(), "pong");

    // Advance epoch → same executor.echo → staleEpoch error
    tx.send(epoch2).unwrap();
    let mut echo_req2 = executor.echo_request();
    echo_req2.get().set_message("hello");
    match echo_req2.send().promise.await {
        Ok(_) => panic!("echo should fail with RPC error after epoch advance"),
        Err(e) => assert!(
            e.to_string().contains("staleEpoch"),
            "error should mention staleEpoch, got: {e}"
        ),
    }
}

/// No-chain regression test: Membrane graft works without auth (pure ocap).
#[tokio::test]
async fn test_membrane_graft_no_auth() {
    let epoch = Epoch {
        seq: 1,
        head: b"head1".to_vec(),
        adopted_block: 100,
    };

    let (_tx, rx) = watch::channel(epoch);
    let membrane = stub_membrane(rx);

    // graft() is parameterless — having the reference IS authorization.
    let graft_resp = membrane
        .graft_request()
        .send()
        .promise
        .await
        .expect("graft RPC");
    let results = graft_resp.get().expect("graft results");
    let executor = results.get_executor().expect("executor");

    let mut echo_req = executor.echo_request();
    echo_req.get().set_message("ping");
    let echo_resp = echo_req.send().promise.await.expect("echo RPC");
    let response = echo_resp
        .get()
        .expect("echo results")
        .get_response()
        .expect("response");
    assert_eq!(response.to_str().unwrap(), "pong");
}

/// No-chain: echo fails with staleEpoch after epoch advance, then re-graft recovers.
#[tokio::test]
async fn test_membrane_stale_epoch_then_recovery_no_chain() {
    let epoch1 = Epoch {
        seq: 1,
        head: b"head1".to_vec(),
        adopted_block: 100,
    };
    let epoch2 = Epoch {
        seq: 2,
        head: b"head2".to_vec(),
        adopted_block: 101,
    };

    let (tx, rx) = watch::channel(epoch1.clone());
    let membrane = stub_membrane(rx);

    // Graft → echo → Ok
    let graft_resp = membrane
        .graft_request()
        .send()
        .promise
        .await
        .expect("graft RPC");
    let executor = graft_resp
        .get()
        .expect("graft results")
        .get_executor()
        .expect("executor");

    let mut echo_req = executor.echo_request();
    echo_req.get().set_message("ping");
    let echo_resp = echo_req.send().promise.await.expect("echo RPC");
    let response = echo_resp
        .get()
        .expect("echo results")
        .get_response()
        .expect("response");
    assert_eq!(
        response.to_str().unwrap(),
        "pong",
        "graft should be ok under current epoch"
    );

    // Advance epoch → same executor.echo → staleEpoch
    tx.send(epoch2).unwrap();
    let mut echo_req2 = executor.echo_request();
    echo_req2.get().set_message("ping");
    match echo_req2.send().promise.await {
        Ok(_) => panic!("echo should fail with RPC error after epoch advance"),
        Err(e) => assert!(
            e.to_string().contains("staleEpoch"),
            "error should mention staleEpoch, got: {e}"
        ),
    }

    // Re-graft → new executor.echo → Ok
    let graft_resp2 = membrane
        .graft_request()
        .send()
        .promise
        .await
        .expect("re-graft RPC");
    let executor2 = graft_resp2
        .get()
        .expect("re-graft results")
        .get_executor()
        .expect("executor");

    let mut echo_req3 = executor2.echo_request();
    echo_req3.get().set_message("ping");
    let echo_resp3 = echo_req3
        .send()
        .promise
        .await
        .expect("echo after re-graft RPC");
    let response3 = echo_resp3
        .get()
        .expect("echo results")
        .get_response()
        .expect("response");
    assert_eq!(response3.to_str().unwrap(), "pong", "re-graft should be ok");
}

/// Terminal login with wrong key should fail authentication.
#[tokio::test]
async fn test_terminal_wrong_key_rejected() {
    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };

    // Terminal expects key A, signer holds key B.
    let sk_a = SigningKey::generate(&mut rand::rngs::OsRng);
    let sk_b = SigningKey::generate(&mut rand::rngs::OsRng);
    let vk_a = sk_a.verifying_key();

    let (_tx, rx) = watch::channel(epoch);
    let terminal = terminal_membrane(rx, vk_a);
    let signer_client: stem_capnp::signer::Client = new_client(TestSigner::from_ed25519(&sk_b));

    let mut login_req = terminal.login_request();
    login_req.get().set_signer(signer_client);

    match login_req.send().promise.await {
        Ok(resp) => match resp.get() {
            Ok(_) => panic!("login should fail with wrong key"),
            Err(e) => assert!(
                e.to_string().contains("login auth failed"),
                "error should mention login auth failure, got: {e}"
            ),
        },
        Err(e) => assert!(
            e.to_string().contains("login auth failed"),
            "error should mention login auth failure, got: {e}"
        ),
    }
}

/// Terminal login without signer should fail.
#[tokio::test]
async fn test_terminal_missing_signer_rejected() {
    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };

    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let vk = sk.verifying_key();

    let (_tx, rx) = watch::channel(epoch);
    let terminal = terminal_membrane(rx, vk);

    // Call login without setting signer.
    let login_req = terminal.login_request();
    match login_req.send().promise.await {
        Ok(resp) => match resp.get() {
            Ok(_) => panic!("login should fail without signer"),
            Err(e) => assert!(
                e.to_string().contains("missing signer"),
                "error should mention missing signer, got: {e}"
            ),
        },
        Err(e) => assert!(
            e.to_string().contains("missing signer"),
            "error should mention missing signer, got: {e}"
        ),
    }
}

/// Helper: create a Membrane client with all 5 capabilities populated.
fn full_stub_membrane(rx: watch::Receiver<Epoch>) -> stem_capnp::membrane::Client {
    new_client(MembraneServer::new(rx, FullStubSessionBuilder))
}

/// Verify that graft() returns all 5 capabilities: identity, host, executor, ipfs, routing.
#[tokio::test]
async fn test_graft_returns_all_five_capabilities() {
    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };

    let (_tx, rx) = watch::channel(epoch);
    let membrane = full_stub_membrane(rx);

    let graft_resp = membrane
        .graft_request()
        .send()
        .promise
        .await
        .expect("graft RPC");
    let results = graft_resp.get().expect("graft results");

    // All 5 capability fields must be non-null.
    results
        .get_identity()
        .expect("identity capability should be present");
    results
        .get_host()
        .expect("host capability should be present");
    results
        .get_executor()
        .expect("executor capability should be present");
    results
        .get_ipfs()
        .expect("ipfs capability should be present");
    results
        .get_routing()
        .expect("routing capability should be present");

    // Verify executor actually works (echo).
    let executor = results.get_executor().expect("executor");
    let mut echo_req = executor.echo_request();
    echo_req.get().set_message("all-caps");
    let echo_resp = echo_req.send().promise.await.expect("echo RPC");
    let response = echo_resp
        .get()
        .expect("echo results")
        .get_response()
        .expect("response");
    assert_eq!(response.to_str().unwrap(), "pong");
}

/// Test Terminal-gated Membrane over a VatNetwork stream pair (simulates the
/// libp2p `/ww/0.1.0` path from `serve_one_terminal_stream` in executor.rs).
///
/// Server side: bootstrap = Terminal(Membrane).
/// Client side: bootstrap Terminal → login(signer) → get Membrane → graft → echo.
#[tokio::test]
async fn test_terminal_over_stream_pair() {
    use capnp_rpc::rpc_twoparty_capnp::Side;
    use capnp_rpc::twoparty::VatNetwork;
    use capnp_rpc::RpcSystem;
    use futures::AsyncReadExt;

    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let vk = sk.verifying_key();
    let (_tx, rx) = watch::channel(epoch);
    let membrane = full_stub_membrane(rx);

    let terminal = TerminalServer::<stem_capnp::membrane::Owned>::new(
        vk,
        membrane,
        SigningDomain::terminal_membrane(),
    );
    let terminal_client: stem_capnp::terminal::Client<stem_capnp::membrane::Owned> =
        new_client(terminal);

    let (client_stream, server_stream) = tokio::io::duplex(4096);

    // RpcSystem is !Send, so we need a LocalSet.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let (sr, sw) =
                tokio_util::compat::TokioAsyncReadCompatExt::compat(server_stream).split();
            let server_network = VatNetwork::new(sr, sw, Side::Server, Default::default());
            let server_rpc = RpcSystem::new(Box::new(server_network), Some(terminal_client.client));

            let (cr, cw) =
                tokio_util::compat::TokioAsyncReadCompatExt::compat(client_stream).split();
            let client_network = VatNetwork::new(cr, cw, Side::Client, Default::default());
            let mut client_rpc =
                RpcSystem::new(Box::new(client_network), None::<capnp::capability::Client>);
            let remote_terminal: stem_capnp::terminal::Client<stem_capnp::membrane::Owned> =
                client_rpc.bootstrap(Side::Server);

            tokio::task::spawn_local(async move {
                let _ = server_rpc.await;
            });
            tokio::task::spawn_local(async move {
                let _ = client_rpc.await;
            });

            // Login with correct signer → get Membrane → graft → echo.
            let signer_client: stem_capnp::signer::Client =
                new_client(TestSigner::from_ed25519(&sk));
            let mut login_req = remote_terminal.login_request();
            login_req.get().set_signer(signer_client);

            let login_resp = timeout(Duration::from_secs(5), login_req.send().promise)
                .await
                .expect("login timed out")
                .expect("login RPC");

            let remote_membrane: stem_capnp::membrane::Client = login_resp
                .get()
                .expect("login results")
                .get_session()
                .expect("session");

            let graft_resp = timeout(
                Duration::from_secs(5),
                remote_membrane.graft_request().send().promise,
            )
            .await
            .expect("graft timed out")
            .expect("graft RPC");

            let results = graft_resp.get().expect("graft results");
            let executor = results.get_executor().expect("executor");
            let mut echo_req = executor.echo_request();
            echo_req.get().set_message("over-the-wire");
            let echo_resp = timeout(Duration::from_secs(5), echo_req.send().promise)
                .await
                .expect("echo timed out")
                .expect("echo RPC");
            let response = echo_resp
                .get()
                .expect("echo results")
                .get_response()
                .expect("response");
            assert_eq!(response.to_str().unwrap(), "pong");
        })
        .await;
}

/// Test that Terminal-over-stream rejects login with wrong key.
#[tokio::test]
async fn test_terminal_over_stream_wrong_key_rejected() {
    use capnp_rpc::rpc_twoparty_capnp::Side;
    use capnp_rpc::twoparty::VatNetwork;
    use capnp_rpc::RpcSystem;
    use futures::AsyncReadExt;

    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };
    let host_sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let host_vk = host_sk.verifying_key();
    let wrong_sk = SigningKey::generate(&mut rand::rngs::OsRng);

    let (_tx, rx) = watch::channel(epoch);
    let membrane = full_stub_membrane(rx);

    let terminal = TerminalServer::<stem_capnp::membrane::Owned>::new(
        host_vk,
        membrane,
        SigningDomain::terminal_membrane(),
    );
    let terminal_client: stem_capnp::terminal::Client<stem_capnp::membrane::Owned> =
        new_client(terminal);

    let (client_stream, server_stream) = tokio::io::duplex(4096);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let (sr, sw) =
                tokio_util::compat::TokioAsyncReadCompatExt::compat(server_stream).split();
            let server_network = VatNetwork::new(sr, sw, Side::Server, Default::default());
            let server_rpc = RpcSystem::new(Box::new(server_network), Some(terminal_client.client));

            let (cr, cw) =
                tokio_util::compat::TokioAsyncReadCompatExt::compat(client_stream).split();
            let client_network = VatNetwork::new(cr, cw, Side::Client, Default::default());
            let mut client_rpc =
                RpcSystem::new(Box::new(client_network), None::<capnp::capability::Client>);
            let remote_terminal: stem_capnp::terminal::Client<stem_capnp::membrane::Owned> =
                client_rpc.bootstrap(Side::Server);

            tokio::task::spawn_local(async move {
                let _ = server_rpc.await;
            });
            tokio::task::spawn_local(async move {
                let _ = client_rpc.await;
            });

            // Login with wrong key — should fail.
            let signer_client: stem_capnp::signer::Client =
                new_client(TestSigner::from_ed25519(&wrong_sk));
            let mut login_req = remote_terminal.login_request();
            login_req.get().set_signer(signer_client);

            let result = timeout(Duration::from_secs(5), login_req.send().promise).await;
            match result {
                Ok(Ok(resp)) => match resp.get() {
                    Ok(_) => panic!("login should fail with wrong key"),
                    Err(e) => assert!(
                        e.to_string().contains("login auth failed"),
                        "expected login auth failure error, got: {e}"
                    ),
                },
                Ok(Err(e)) => assert!(
                    e.to_string().contains("login auth failed"),
                    "expected login auth failure error, got: {e}"
                ),
                Err(_) => panic!("login timed out — expected auth failure"),
            }
        })
        .await;
}

/// Signer that returns malformed signature bytes.
struct MalformedSigner;

#[allow(refining_impl_trait)]
impl stem_capnp::signer::Server for MalformedSigner {
    fn sign(
        self: capnp::capability::Rc<Self>,
        _params: stem_capnp::signer::SignParams,
        mut results: stem_capnp::signer::SignResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        // 64 bytes of 0xFF is not a valid signed envelope.
        results.get().set_sig(&[0xFF; 64]);
        capnp::capability::Promise::ok(())
    }
}

/// Terminal should reject login when the signer returns malformed signature bytes.
#[tokio::test]
async fn test_terminal_malformed_signature_rejected() {
    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };

    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let vk = sk.verifying_key();

    let (_tx, rx) = watch::channel(epoch);
    let terminal = terminal_membrane(rx, vk);
    let signer_client: stem_capnp::signer::Client = new_client(MalformedSigner);

    let mut login_req = terminal.login_request();
    login_req.get().set_signer(signer_client);

    match login_req.send().promise.await {
        Ok(resp) => match resp.get() {
            Ok(_) => panic!("login should fail with malformed signature"),
            Err(e) => assert!(
                e.to_string().contains("invalid signed envelope"),
                "error should mention invalid signed envelope, got: {e}"
            ),
        },
        Err(e) => assert!(
            e.to_string().contains("invalid signed envelope"),
            "error should mention invalid signed envelope, got: {e}"
        ),
    }
}
