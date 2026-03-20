//! Terminal server: challenge-response auth gate for capability access.
//!
//! `Terminal(Session)` is the authentication boundary. The caller must prove
//! identity by signing a nonce with the expected key. On success, the guarded
//! session capability is returned.
//!
//! This separates authentication (Terminal) from capability provisioning
//! (Membrane). Having a Membrane reference IS authorization (ocap); Terminal
//! is the gate that decides who gets that reference.

use crate::stem_capnp;
use auth::SigningDomain;
use capnp::capability::Promise;
use capnp::Error;
use capnp_rpc::pry;
use k256::ecdsa::VerifyingKey;

/// Authentication gate that guards access to a capability via challenge-response.
///
/// Generic over the session type — typically `Terminal<membrane::Owned>`, but
/// can wrap any Cap'n Proto capability interface.
///
/// # Auth flow
///
/// 1. Caller sends `login(signer)` request
/// 2. Terminal generates a random nonce, sends `signer.sign(nonce)`
/// 3. Terminal verifies the signature against its `verifying_key`
/// 4. On success, returns the guarded `session` capability
pub struct TerminalServer<Session: capnp::traits::Owned> {
    verifying_key: VerifyingKey,
    session: <Session as capnp::traits::Owned>::Reader<'static>,
}

impl<Session> TerminalServer<Session>
where
    Session: capnp::traits::Owned,
    <Session as capnp::traits::Owned>::Reader<'static>: Clone,
{
    /// Create a new Terminal guarding the given session capability.
    pub fn new(
        vk: VerifyingKey,
        session: <Session as capnp::traits::Owned>::Reader<'static>,
    ) -> Self {
        Self {
            verifying_key: vk,
            session,
        }
    }
}

#[allow(refining_impl_trait)]
impl<Session> stem_capnp::terminal::Server<Session> for TerminalServer<Session>
where
    Session: capnp::traits::Owned + 'static,
    <Session as capnp::traits::Owned>::Reader<'static>: capnp::traits::SetterInput<Session> + Clone,
{
    fn login(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::terminal::LoginParams<Session>,
        mut results: stem_capnp::terminal::LoginResults<Session>,
    ) -> Promise<(), Error> {
        let signer: stem_capnp::signer::Client = match pry!(params.get()).get_signer() {
            Ok(s) => s,
            Err(_) => return Promise::err(Error::failed("missing signer".into())),
        };

        let vk = self.verifying_key;
        let session = self.session.clone();

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
                Error::failed("login auth failed: signature verification failed".into())
            })?;

            results.get().set_session(session)?;
            Ok(())
        })
    }
}
