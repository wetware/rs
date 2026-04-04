# Stem schema: Epoch, Signer, Identity, and Membrane definitions.
# Compiled by the membrane crate (crates/membrane/build.rs).
# The host re-exports generated types via `pub use membrane::stem_capnp`.

@0x9bce094a026970c4;

struct Epoch {
  seq @0 :UInt64;        # Monotonic epoch sequence number (from Stem.seq).
  head @1 :Data;         # Opaque head bytes from the Stem contract.
  adoptedBlock @2 :UInt64;# Block number at which this epoch was adopted.
}

interface Signer {
  sign @0 (nonce :UInt64) -> (sig :Data);
  # Sign a nonce; the domain and payload_type are baked in by the issuing Identity hub.
}

interface Identity {
  # Returns a Signer scoped to the requested signing domain.
  signer @0 (domain :Text) -> (signer :Signer);

  verify @1 (data :Data, signature :Data, pubkey :Data) -> (valid :Bool);
  # Verify an Ed25519 signature against an arbitrary public key.
  # Stateless — does not use the node's private key.
  # The pubkey is the 32-byte Ed25519 verifying key.
  # The signature is the 64-byte Ed25519 signature.
}

interface Terminal(Session) {
  login @0 (signer :Signer) -> (session :Session);
  # Authenticate via challenge-response, returning the guarded capability.
  # Having a Terminal reference does NOT grant access — the caller must prove
  # identity by signing a nonce with the expected key.
}

struct NamedCap {
  name @0 :Text;
  cap  @1 :AnyPointer;
  schema @2 :Data;
  # A named, typed, type-erased capability.
  # name: canonical name (e.g. "host", "identity", "runtime").
  # cap: the capability itself (cast via get_as()).
  # schema: Cap'n Proto schema bytes for runtime introspection (may be empty).
}

interface Membrane {
  graft @0 () -> (
    caps :List(NamedCap)       # All capabilities, named and type-erased.
  );
  # Pure capability provisioning (ocap model). Having a Membrane reference IS
  # authorization — no signer needed. Wrap in Terminal(Membrane) to gate access.
  #
  # Canonical names: "identity", "host", "runtime", "routing", "http-client".
  # Init.d-scoped grants (from `with` blocks) are appended after the core caps.
  #
  # Listener/Dialer accessed via host.network().
  # IPFS content access goes through the WASI virtual filesystem (CidTree).
}
