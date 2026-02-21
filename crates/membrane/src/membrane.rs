//! Membrane server: issues epoch-scoped sessions via `graft()`.

use crate::epoch::{Epoch, EpochGuard};
use crate::stem_capnp;
use capnp::capability::Promise;
use capnp::Error;
use capnp_rpc::new_client;
use tokio::sync::watch;

/// Callback trait for filling the concrete Session during graft.
///
/// Implementors receive the EpochGuard and a builder for the session,
/// allowing platform-specific capabilities (Host, Executor, IPFS) to be
/// injected into the session fields.
pub trait SessionBuilder: 'static {
    fn build(
        &self,
        guard: &EpochGuard,
        builder: stem_capnp::session::Builder<'_>,
    ) -> Result<(), Error>;
}

/// No-op session builder: leaves all session fields empty.
///
/// Useful for testing or guests that don't need platform capabilities.
pub struct NoExtension;

impl SessionBuilder for NoExtension {
    fn build(
        &self,
        _guard: &EpochGuard,
        _builder: stem_capnp::session::Builder<'_>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

/// Membrane server: stable across epochs, backed by a watch receiver for the adopted epoch.
///
/// The `session_builder` callback fills the session fields when a session is issued.
pub struct MembraneServer<F: SessionBuilder> {
    receiver: watch::Receiver<Epoch>,
    session_builder: F,
}

impl<F: SessionBuilder> MembraneServer<F> {
    pub fn new(receiver: watch::Receiver<Epoch>, session_builder: F) -> Self {
        Self {
            receiver,
            session_builder,
        }
    }

    fn get_current_epoch(&self) -> Epoch {
        self.receiver.borrow().clone()
    }
}

#[allow(refining_impl_trait)]
impl<F: SessionBuilder> stem_capnp::membrane::Server for MembraneServer<F> {
    fn graft(
        self: capnp::capability::Rc<Self>,
        _params: stem_capnp::membrane::GraftParams,
        mut results: stem_capnp::membrane::GraftResults,
    ) -> Promise<(), Error> {
        let epoch = self.get_current_epoch();
        let guard = EpochGuard {
            issued_seq: epoch.seq,
            receiver: self.receiver.clone(),
        };
        let session_builder = results.get().init_session();
        if let Err(e) = self.session_builder.build(&guard, session_builder) {
            return Promise::err(e);
        }
        Promise::ok(())
    }
}

/// StatusPoller server: epoch-scoped; pollStatus returns an RPC error when the
/// epoch has advanced past the one under which this capability was issued.
pub struct StatusPollerServer {
    pub guard: EpochGuard,
}

/// Builds a Membrane capability client from a watch receiver (for use over capnp-rpc).
///
/// Uses `NoExtension` — all session fields are left empty (null capabilities).
/// For platform-specific sessions, construct
/// `MembraneServer::new(receiver, your_session_builder)` directly.
pub fn membrane_client(receiver: watch::Receiver<Epoch>) -> stem_capnp::membrane::Client {
    new_client(MembraneServer::new(receiver, NoExtension))
}
