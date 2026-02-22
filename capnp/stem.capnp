# Stem schema: Epoch, Signer, Identity, Session, and Membrane definitions.
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
  #
  # The host validates `domain` against `protocol::SigningDomain` (crates/protocol).
  # Unknown strings are rejected with an error; new domains must be added there
  # explicitly so the set stays finite and auditable.
  signer @0 (domain :Text) -> (signer :Signer);
}

struct Session {
  host     @0 :import "system.capnp".Host;      # Swarm-level operations (id, addrs, peers, connect).
  executor @1 :import "system.capnp".Executor;  # WASM execution (runBytes, echo).
  ipfs     @2 :import "ipfs.capnp".Client;      # IPFS CoreAPI (unixfs, block, dag, ...).
  identity @3 :Identity;                        # Host-side identity hub: maps signing domains → Signers.
}


interface Membrane {
  graft @0 (signer :Signer) -> (session :Session);
  # Graft a signer to the membrane, establishing an epoch-scoped session.
}
