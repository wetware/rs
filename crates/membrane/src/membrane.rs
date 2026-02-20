//! Membrane server: issues epoch-scoped sessions via `graft()`.

use crate::epoch::{fill_epoch_builder, Epoch, EpochGuard};
use crate::stem_capnp;
use capnp::capability::Promise;
use capnp::Error;
use capnp_rpc::new_client;
use tokio::sync::watch;

/// Callback trait for filling the session extension during graft.
///
/// Implementors receive the EpochGuard and a builder for the extension field,
/// allowing platform-specific capabilities (e.g. Host, Executor) to be injected
/// into the session.
pub trait SessionExtensionBuilder<SessionExt>: 'static
where
    SessionExt: capnp::traits::Owned,
{
    fn build(
        &self,
        guard: &EpochGuard,
        builder: <SessionExt as capnp::traits::Owned>::Builder<'_>,
    ) -> Result<(), Error>;
}

/// No-op extension builder for sessions without platform-specific capabilities.
pub struct NoExtension;

impl SessionExtensionBuilder<capnp::any_pointer::Owned> for NoExtension {
    fn build(
        &self,
        _guard: &EpochGuard,
        _builder: capnp::any_pointer::Builder<'_>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

/// Membrane server: stable across epochs, backed by a watch receiver for the adopted epoch.
///
/// Generic over `SessionExt`: the type parameter for the Session's extension field.
/// The `ext_builder` callback fills the extension when a session is issued.
pub struct MembraneServer<SessionExt, F>
where
    SessionExt: capnp::traits::Owned,
    F: SessionExtensionBuilder<SessionExt>,
{
    receiver: watch::Receiver<Epoch>,
    ext_builder: F,
    _phantom: std::marker::PhantomData<SessionExt>,
}

impl<SessionExt, F> MembraneServer<SessionExt, F>
where
    SessionExt: capnp::traits::Owned,
    F: SessionExtensionBuilder<SessionExt>,
{
    pub fn new(receiver: watch::Receiver<Epoch>, ext_builder: F) -> Self {
        Self {
            receiver,
            ext_builder,
            _phantom: std::marker::PhantomData,
        }
    }

    fn get_current_epoch(&self) -> Epoch {
        self.receiver.borrow().clone()
    }
}

#[allow(refining_impl_trait)]
impl<SessionExt, F> stem_capnp::membrane::Server<SessionExt> for MembraneServer<SessionExt, F>
where
    SessionExt: capnp::traits::Owned + 'static,
    F: SessionExtensionBuilder<SessionExt>,
{
    fn graft(
        self: capnp::capability::Rc<Self>,
        _params: stem_capnp::membrane::GraftParams<SessionExt>,
        mut results: stem_capnp::membrane::GraftResults<SessionExt>,
    ) -> Promise<(), Error> {
        let epoch = self.get_current_epoch();
        let mut session_builder = results.get().init_session();
        if fill_epoch_builder(&mut session_builder.reborrow().init_issued_epoch(), &epoch).is_err()
        {
            return Promise::err(Error::failed("fill issued epoch".to_string()));
        }
        let guard = EpochGuard {
            issued_seq: epoch.seq,
            receiver: self.receiver.clone(),
        };
        let poller = StatusPollerServer {
            guard: guard.clone(),
        };
        session_builder
            .reborrow()
            .set_status_poller(new_client(poller));

        if let Err(e) = self
            .ext_builder
            .build(&guard, session_builder.reborrow().init_extension())
        {
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

#[allow(refining_impl_trait)]
impl stem_capnp::status_poller::Server for StatusPollerServer {
    fn poll_status(
        self: capnp::capability::Rc<Self>,
        _: stem_capnp::status_poller::PollStatusParams,
        mut results: stem_capnp::status_poller::PollStatusResults,
    ) -> Promise<(), Error> {
        if let Err(e) = self.guard.check() {
            return Promise::err(e);
        }
        results.get().set_status(stem_capnp::Status::Ok);
        Promise::ok(())
    }
}

/// Builds a Membrane capability client from a watch receiver (for use over capnp-rpc).
///
/// Uses `NoExtension` -- the session's extension field is left empty.
/// For platform-specific extensions, construct
/// `MembraneServer::new(receiver, your_ext_builder)` directly.
pub fn membrane_client(
    receiver: watch::Receiver<Epoch>,
) -> stem_capnp::membrane::Client<capnp::any_pointer::Owned> {
    new_client(MembraneServer::new(receiver, NoExtension))
}
