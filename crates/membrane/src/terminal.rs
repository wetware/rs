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
use ed25519_dalek::VerifyingKey;
use libp2p_core::SignedEnvelope;

/// Authentication gate that guards access to a capability via challenge-response.
///
/// Generic over the session type — typically `Terminal<membrane::Owned>`, but
/// can wrap any Cap'n Proto capability interface.
///
/// # Auth flow
///
/// 1. Caller sends `login(signer)` request
/// 2. Terminal generates a random nonce, sends `signer.sign(nonce)`
/// 3. Signer returns a libp2p `SignedEnvelope` (RFC 0002) containing the nonce
/// 4. Terminal decodes the envelope, verifies the signature + domain + nonce,
///    and checks the signing key matches the expected `verifying_key`
/// 5. On success, returns the guarded `session` capability
pub struct TerminalServer<Session: capnp::traits::Owned> {
    verifying_key: VerifyingKey,
    session: <Session as capnp::traits::Owned>::Reader<'static>,
    domain: SigningDomain,
}

impl<Session> TerminalServer<Session>
where
    Session: capnp::traits::Owned,
    <Session as capnp::traits::Owned>::Reader<'static>: Clone,
{
    /// Create a new Terminal guarding the given session capability.
    ///
    /// The `domain` determines the signing context for challenge-response auth.
    /// Different guarded capabilities should use different domains to prevent
    /// cross-protocol signature replay.
    pub fn new(
        vk: VerifyingKey,
        session: <Session as capnp::traits::Owned>::Reader<'static>,
        domain: SigningDomain,
    ) -> Self {
        Self {
            verifying_key: vk,
            session,
            domain,
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
        let domain = self.domain.clone();

        let nonce: u64 = rand::random();
        let mut sign_req = signer.sign_request();
        sign_req.get().set_nonce(nonce);

        Promise::from_future(async move {
            let sign_resp = sign_req.send().promise.await?;
            let sig_bytes = sign_resp.get()?.get_sig()?;

            // Decode the libp2p SignedEnvelope (RFC 0002).
            let envelope = SignedEnvelope::from_protobuf_encoding(sig_bytes)
                .map_err(|e| Error::failed(format!("invalid signed envelope: {e}")))?;

            // Verify signature and extract payload + signing key.
            // This checks domain separation and payload type in one step.
            let (payload, pubkey) = envelope
                .payload_and_signing_key(domain.as_str().to_string(), domain.payload_type())
                .map_err(|e| Error::failed(format!("login auth failed: {e}")))?;

            // Check the nonce matches our challenge.
            let expected_payload = nonce.to_be_bytes();
            if payload != expected_payload {
                return Err(Error::failed("login auth failed: nonce mismatch".into()));
            }

            // Check the signing key matches the expected verifying key.
            let envelope_ed = pubkey
                .clone()
                .try_into_ed25519()
                .map_err(|_| Error::failed("login auth failed: not an ed25519 key".into()))?;
            if envelope_ed.to_bytes() != vk.to_bytes() {
                return Err(Error::failed(
                    "login auth failed: signing key does not match expected identity".into(),
                ));
            }

            results.get().set_session(session)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn terminal_server_constructs_with_membrane_owned() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();

        // Build a null membrane client to use as session.
        let (_tx, rx) = tokio::sync::watch::channel(crate::epoch::Epoch {
            seq: 1,
            head: vec![],
            adopted_block: 0,
        });
        let membrane: stem_capnp::membrane::Client = crate::membrane::membrane_client(rx);

        let _terminal = TerminalServer::<stem_capnp::membrane::Owned>::new(
            vk,
            membrane,
            SigningDomain::terminal_membrane(),
        );
    }

    #[test]
    fn terminal_server_verifying_key_is_stored() {
        let sk = SigningKey::generate(&mut rand::rngs::OsRng);
        let vk = sk.verifying_key();

        let (_tx, rx) = tokio::sync::watch::channel(crate::epoch::Epoch {
            seq: 1,
            head: vec![],
            adopted_block: 0,
        });
        let membrane: stem_capnp::membrane::Client = crate::membrane::membrane_client(rx);

        let terminal = TerminalServer::<stem_capnp::membrane::Owned>::new(
            vk,
            membrane,
            SigningDomain::terminal_membrane(),
        );
        assert_eq!(terminal.verifying_key, vk);
    }
}
