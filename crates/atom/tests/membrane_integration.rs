//! Integration test: Epoch → Membrane → graft → executor.echo (epoch-guarded).
//! All local: membrane server and client are in-process (capnp-rpc local dispatch).

mod common;

use atom::stem_capnp;
use atom::{AtomIndexer, Epoch, IndexerConfig, MembraneServer};
use capnp_rpc::new_client;
use common::{deploy_atom, set_head, spawn_anvil, StubSessionBuilder};
use k256::ecdsa::SigningKey;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use auth::SigningDomain;
use tokio::sync::watch;
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;

/// Signer that produces real secp256k1 ECDSA signatures for graft challenge-response.
///
/// Constructs the domain-separated signing buffer (matching the host verifier) and
/// returns the 64-byte compact ECDSA signature.
struct TestSigner {
    sk: SigningKey,
}

#[allow(refining_impl_trait)]
impl stem_capnp::signer::Server for TestSigner {
    fn sign(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::signer::SignParams,
        mut results: stem_capnp::signer::SignResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let nonce = capnp_rpc::pry!(params.get()).get_nonce();
        let signing_buffer = SigningDomain::MembraneGraft.signing_buffer(&nonce.to_be_bytes());

        use k256::ecdsa::signature::Signer;
        let signature: k256::ecdsa::Signature = self.sk.sign(&signing_buffer);
        results.get().set_sig(signature.to_bytes().as_slice());
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

/// Helper: create an authenticated membrane client backed by StubSessionBuilder.
fn auth_membrane(
    rx: watch::Receiver<Epoch>,
    vk: k256::ecdsa::VerifyingKey,
) -> stem_capnp::membrane::Client {
    new_client(MembraneServer::new(rx, StubSessionBuilder).with_verifying_key(vk))
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

    // Generate a test keypair for graft authentication.
    let sk = SigningKey::random(&mut rand::thread_rng());
    let vk = *sk.verifying_key();

    let (tx, rx) = watch::channel(epoch1.clone());
    let membrane = auth_membrane(rx, vk);
    let signer_client: stem_capnp::signer::Client = new_client(TestSigner { sk });

    // Graft → echo → Ok
    let mut graft_req = membrane.graft_request();
    graft_req.get().set_signer(signer_client);

    let graft_rpc_response = graft_req.send().promise.await.expect("graft RPC");
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

/// No-chain regression test: echo fails with staleEpoch after epoch advance, then re-graft recovers.
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

    let sk = SigningKey::random(&mut rand::thread_rng());
    let vk = *sk.verifying_key();

    let (tx, rx) = watch::channel(epoch1.clone());
    let membrane = auth_membrane(rx, vk);
    let signer_client: stem_capnp::signer::Client = new_client(TestSigner { sk });

    // Graft → echo → Ok
    let mut graft_req = membrane.graft_request();
    graft_req.get().set_signer(signer_client.clone());
    let graft_rpc_response = graft_req.send().promise.await.expect("graft RPC");
    let graft_response = graft_rpc_response.get().expect("graft results");
    let executor = graft_response.get_executor().expect("executor");

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
    let mut graft_req2 = membrane.graft_request();
    graft_req2.get().set_signer(signer_client.clone());
    let graft_rpc_response2 = graft_req2.send().promise.await.expect("re-graft RPC");
    let graft_response2 = graft_rpc_response2.get().expect("re-graft results");
    let executor2 = graft_response2.get_executor().expect("executor");

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

/// Graft with wrong key should fail authentication.
#[tokio::test]
async fn test_membrane_graft_wrong_key_rejected() {
    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };

    // Membrane expects key A, signer holds key B.
    let sk_a = SigningKey::random(&mut rand::thread_rng());
    let sk_b = SigningKey::random(&mut rand::thread_rng());
    let vk_a = *sk_a.verifying_key();

    let (_tx, rx) = watch::channel(epoch);
    let membrane = auth_membrane(rx, vk_a);
    let signer_client: stem_capnp::signer::Client = new_client(TestSigner { sk: sk_b });

    let mut graft_req = membrane.graft_request();
    graft_req.get().set_signer(signer_client);

    match graft_req.send().promise.await {
        Ok(resp) => match resp.get() {
            Ok(_) => panic!("graft should fail with wrong key"),
            Err(e) => assert!(
                e.to_string().contains("graft auth failed"),
                "error should mention auth failure, got: {e}"
            ),
        },
        Err(e) => assert!(
            e.to_string().contains("graft auth failed"),
            "error should mention auth failure, got: {e}"
        ),
    }
}

/// Graft without signer should fail.
#[tokio::test]
async fn test_membrane_graft_missing_signer_rejected() {
    let epoch = Epoch {
        seq: 1,
        head: b"head".to_vec(),
        adopted_block: 100,
    };

    let sk = SigningKey::random(&mut rand::thread_rng());
    let vk = *sk.verifying_key();

    let (_tx, rx) = watch::channel(epoch);
    let membrane = auth_membrane(rx, vk);

    // Call graft without setting signer.
    let graft_req = membrane.graft_request();
    match graft_req.send().promise.await {
        Ok(resp) => match resp.get() {
            Ok(_) => panic!("graft should fail without signer"),
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
