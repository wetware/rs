//! Membrane server: issues epoch-scoped capabilities via `graft()`.

use crate::epoch::{Epoch, EpochGuard};
use crate::stem_capnp;
use auth::SigningDomain;
use capnp::capability::Promise;
use capnp::Error;
use capnp_rpc::{new_client, pry};
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
/// When `verifying_key` is set, `graft()` performs challenge-response authentication:
/// the caller's signer is challenged with a random nonce, and the returned signature
/// is verified against the configured public key before issuing capabilities.
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

    /// Build epoch-guarded capabilities into the graft results.
    fn build_graft(&self, results: &mut stem_capnp::membrane::GraftResults) -> Result<(), Error> {
        let epoch = self.get_current_epoch();
        let guard = EpochGuard {
            issued_seq: epoch.seq,
            receiver: self.receiver.clone(),
        };
        self.graft_builder.build(&guard, results.get())
    }
}

#[allow(refining_impl_trait)]
impl<F: GraftBuilder> stem_capnp::membrane::Server for MembraneServer<F> {
    fn graft(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::membrane::GraftParams,
        mut results: stem_capnp::membrane::GraftResults,
    ) -> Promise<(), Error> {
        tracing::debug!("Membrane graft() called");
        let vk = match self.verifying_key {
            Some(vk) => vk,
            None => {
                // No verifying key configured — skip authentication.
                if let Err(e) = self.build_graft(&mut results) {
                    return Promise::err(e);
                }
                tracing::debug!("Membrane graft() completed (no auth)");
                return Promise::ok(());
            }
        };

        let signer: stem_capnp::signer::Client = match pry!(params.get()).get_signer() {
            Ok(s) => s,
            Err(_) => return Promise::err(Error::failed("missing signer".into())),
        };

        let nonce: u64 = rand::random();
        let mut sign_req = signer.sign_request();
        sign_req.get().set_nonce(nonce);

        Promise::from_future(async move {
            let sign_resp = sign_req.send().promise.await?;
            let sig_bytes = sign_resp.get()?.get_sig()?;

            // Reconstruct the domain-separated signing buffer and verify.
            let signing_buffer = SigningDomain::MembraneGraft.signing_buffer(&nonce.to_be_bytes());
            let signature = k256::ecdsa::Signature::from_slice(sig_bytes)
                .map_err(|e| Error::failed(format!("invalid signature encoding: {e}")))?;

            use k256::ecdsa::signature::Verifier;
            vk.verify(&signing_buffer, &signature).map_err(|_| {
                Error::failed("graft auth failed: signature verification failed".into())
            })?;

            self.build_graft(&mut results)
        })
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
