//! Dialer capability: open outgoing subprotocol streams to remote peers.
//!
//! The `Dialer` capability lets a guest open a libp2p stream to a specific peer
//! on a named subprotocol. The host opens the stream and returns a bidirectional
//! `ByteStream` capability — the guest reads/writes whatever wire protocol it
//! wants directly.

use capnp::capability::Promise;
use capnp_rpc::pry;
use futures::io::AsyncReadExt;
use libp2p::{PeerId, StreamProtocol};
use membrane::EpochGuard;
use tokio::io;
use tokio_util::compat::{FuturesAsyncReadCompatExt, FuturesAsyncWriteCompatExt};

use crate::system_capnp;

use super::{ByteStreamImpl, StreamMode};

pub struct DialerImpl {
    stream_control: libp2p_stream::Control,
    guard: EpochGuard,
}

impl DialerImpl {
    pub fn new(stream_control: libp2p_stream::Control, guard: EpochGuard) -> Self {
        Self {
            stream_control,
            guard,
        }
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::dialer::Server for DialerImpl {
    fn dial(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::dialer::DialParams,
        mut results: system_capnp::dialer::DialResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());

        let params = pry!(params.get());
        let peer_bytes = pry!(params.get_peer()).to_vec();
        let protocol_str = pry!(pry!(params.get_protocol())
            .to_str()
            .map_err(|e| capnp::Error::failed(e.to_string())));

        if protocol_str.is_empty() {
            return Promise::err(capnp::Error::failed(
                "protocol name must not be empty".into(),
            ));
        }
        if protocol_str.contains('/') {
            return Promise::err(capnp::Error::failed(
                "protocol name must not contain '/'".into(),
            ));
        }

        let peer_id = pry!(PeerId::from_bytes(&peer_bytes)
            .map_err(|e| capnp::Error::failed(format!("invalid peer ID: {e}"))));

        let stream_protocol = pry!(StreamProtocol::try_from_owned(format!(
            "/ww/0.1.0/{protocol_str}"
        ))
        .map_err(|e| capnp::Error::failed(format!("invalid protocol: {e}"))));

        let mut control = self.stream_control.clone();

        Promise::from_future(async move {
            tracing::info!(
                peer = %peer_id,
                protocol = %stream_protocol,
                "Dialing subprotocol"
            );

            let stream = control
                .open_stream(peer_id, stream_protocol.clone())
                .await
                .map_err(|e| {
                    capnp::Error::failed(format!(
                        "failed to open stream to {peer_id} on {stream_protocol}: {e}"
                    ))
                })?;

            // Create a duplex pair: guest_side ↔ host_side.
            // The guest reads/writes via ByteStream RPC on guest_side.
            // The host pumps host_side ↔ libp2p stream.
            let (host_side, guest_side) = io::duplex(8 * 1024);

            // Split both sides for bidirectional pumping.
            let (stream_read, stream_write) = Box::pin(stream).split();
            let (mut host_read, mut host_write) = io::split(host_side);

            // Pump: libp2p stream → host_side (remote writes → guest reads)
            tokio::task::spawn_local(async move {
                if let Err(e) = io::copy(&mut stream_read.compat(), &mut host_write).await {
                    tracing::debug!("stream→host pump error: {e}");
                }
            });

            // Pump: host_side → libp2p stream (guest writes → remote reads)
            tokio::task::spawn_local(async move {
                let mut compat_write = stream_write.compat_write();
                if let Err(e) = io::copy(&mut host_read, &mut compat_write).await {
                    tracing::debug!("host→stream pump error: {e}");
                }
            });

            // Wrap guest_side as a bidirectional ByteStream capability.
            let stream_cap: system_capnp::byte_stream::Client =
                capnp_rpc::new_client(ByteStreamImpl::new(guest_side, StreamMode::Bidirectional));
            results.get().set_stream(stream_cap);

            Ok(())
        })
    }
}
