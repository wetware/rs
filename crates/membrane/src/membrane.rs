//! Membrane server: issues epoch-scoped capabilities via `graft()`.

use crate::epoch::{Epoch, EpochGuard};
use crate::stem_capnp;
use capnp::capability::Promise;
use capnp::Error;
use capnp_rpc::new_client;
use k256::ecdsa::VerifyingKey;
use tokio::sync::watch;

/// Callback trait for populating the graft response with capabilities.
///
/// Implementors receive the EpochGuard and a builder for the graft results,
/// allowing platform-specific capabilities (Host, Executor, IPFS) to be
/// injected into the response fields.
pub trait GraftBuilder: 'static {
    fn build(
        &self,
        guard: &EpochGuard,
        builder: stem_capnp::membrane::graft_results::Builder<'_>,
    ) -> Result<(), Error>;
}

/// No-op graft builder: leaves all result fields empty.
///
/// Useful for testing or guests that don't need platform capabilities.
pub struct NoExtension;

impl GraftBuilder for NoExtension {
    fn build(
        &self,
        _guard: &EpochGuard,
        _builder: stem_capnp::membrane::graft_results::Builder<'_>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

/// Membrane server: stable across epochs, backed by a watch receiver for the adopted epoch.
///
/// The `graft_builder` callback fills the result fields when `graft()` is called.
/// When `verifying_key` is set, `graft()` will validate the caller's secp256k1
/// signature against it (implemented in issue #57).
pub struct MembraneServer<F: GraftBuilder> {
    receiver: watch::Receiver<Epoch>,
    graft_builder: F,
    verifying_key: Option<VerifyingKey>,
}

impl<F: GraftBuilder> MembraneServer<F> {
    pub fn new(receiver: watch::Receiver<Epoch>, graft_builder: F) -> Self {
        Self {
            receiver,
            graft_builder,
            verifying_key: None,
        }
    }

    /// Set the secp256k1 verifying key used to authenticate `graft()` callers.
    pub fn with_verifying_key(mut self, vk: VerifyingKey) -> Self {
        self.verifying_key = Some(vk);
        self
    }

    fn get_current_epoch(&self) -> Epoch {
        self.receiver.borrow().clone()
    }
}

#[allow(refining_impl_trait)]
impl<F: GraftBuilder> stem_capnp::membrane::Server for MembraneServer<F> {
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
        if let Err(e) = self.graft_builder.build(&guard, results.get()) {
            return Promise::err(e);
        }
        Promise::ok(())
    }
}

/// Builds a Membrane capability client from a watch receiver (for use over capnp-rpc).
///
/// Uses `NoExtension` — all result fields are left empty (null capabilities).
/// For platform-specific graft responses, construct
/// `MembraneServer::new(receiver, your_graft_builder)` directly.
pub fn membrane_client(receiver: watch::Receiver<Epoch>) -> stem_capnp::membrane::Client {
    new_client(MembraneServer::new(receiver, NoExtension))
}
