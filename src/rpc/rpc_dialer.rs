//! RpcDialer capability: open outgoing subprotocol streams and bootstrap RPC.
//!
//! The `RpcDialer` capability lets a guest dial a remote peer on a named
//! subprotocol and receive the remote's bootstrap capability directly. The
//! host opens the libp2p stream, bootstraps Cap'n Proto RPC over it, and
//! returns the remote's exported capability to the guest.
//!
//! This is the capability-mode counterpart of `Dialer` (byte-stream mode).

use std::time::Duration;

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::io::AsyncReadExt;
use libp2p::PeerId;
use membrane::EpochGuard;

use crate::system_capnp;

/// Timeout for establishing the libp2p stream to a remote peer.
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct RpcDialerImpl {
    stream_control: libp2p_stream::Control,
    guard: EpochGuard,
}

impl RpcDialerImpl {
    pub fn new(stream_control: libp2p_stream::Control, guard: EpochGuard) -> Self {
        Self {
            stream_control,
            guard,
        }
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::rpc_dialer::Server for RpcDialerImpl {
    fn dial(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::rpc_dialer::DialParams,
        mut results: system_capnp::rpc_dialer::DialResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());

        let params = pry!(params.get());
        let peer_bytes = pry!(params.get_peer()).to_vec();
        let schema_bytes = pry!(params.get_schema()).to_vec();

        if schema_bytes.is_empty() {
            return Promise::err(capnp::Error::failed("schema must not be empty".into()));
        }

        let peer_id = pry!(PeerId::from_bytes(&peer_bytes)
            .map_err(|e| capnp::Error::failed(format!("invalid peer ID: {e}"))));

        let protocol_cid = super::schema_cid(&schema_bytes);
        let stream_protocol = pry!(super::schema_protocol(&protocol_cid));

        let mut control = self.stream_control.clone();

        Promise::from_future(async move {
            tracing::debug!(
                peer = %peer_id,
                protocol = %stream_protocol,
                "Dialing RPC subprotocol"
            );

            // Open stream with timeout to avoid hanging on unreachable peers.
            let stream = tokio::time::timeout(
                DIAL_TIMEOUT,
                control.open_stream(peer_id, stream_protocol.clone()),
            )
            .await
            .map_err(|_| {
                capnp::Error::failed(format!(
                    "timeout dialing {peer_id} on {stream_protocol} after {DIAL_TIMEOUT:?}"
                ))
            })?
            .map_err(|e| {
                capnp::Error::failed(format!(
                    "failed to open stream to {peer_id} on {stream_protocol}: {e}"
                ))
            })?;

            // Bootstrap Cap'n Proto RPC over the libp2p stream.
            let (reader, writer) = Box::pin(stream).split();
            let network = VatNetwork::new(reader, writer, Side::Client, Default::default());
            let mut rpc_system = RpcSystem::new(Box::new(network), None);
            let remote_cap: capnp::capability::Client = rpc_system.bootstrap(Side::Server);

            // Drive the RPC system in a background task. Cap'n Proto refcounting
            // handles shutdown: when the guest drops all capabilities obtained from
            // this RPC system, the system drains and the task completes.
            tokio::task::spawn_local(async move {
                let _ = rpc_system.await;
                tracing::debug!(
                    peer = %peer_id,
                    protocol = %stream_protocol,
                    "RPC dial session ended"
                );
            });

            results.get().init_cap().set_as_capability(remote_cap.hook);

            Ok(())
        })
    }
}
