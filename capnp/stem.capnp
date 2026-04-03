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
  # A named, type-erased capability.  Used to forward init.d-scoped
  # grants (via `with` + `cell`) into spawned cells' membranes.
}

interface Membrane {
  graft @0 () -> (
    identity :Identity,                           # Host-side identity hub: maps signing domains → Signers.
    host     :import "system.capnp".Host,         # Swarm-level operations (id, addrs, peers, network).
    runtime  :import "system.capnp".Runtime,      # WASM compilation + execution (load, shutdown).
    routing  :import "routing.capnp".Routing,      # Content routing + data transfer via IPFS.
    httpClient :import "http.capnp".HttpClient,   # Outbound HTTP (domain-scoped).
    extras   :List(NamedCap)                      # Init.d-scoped grants from `with` block.
  );
  # Pure capability provisioning (ocap model). Having a Membrane reference IS
  # authorization — no signer needed. Wrap in Terminal(Membrane) to gate access.
  # Listener/Dialer accessed via host.network().
  # IPFS content access goes through the WASI virtual filesystem (CidTree).
}
