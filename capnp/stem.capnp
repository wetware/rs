# Stem schema: Epoch, Signer, Session, and Membrane definitions.
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
  # Sign an arbitrary nonce under a given domain string.
}

struct Session {
  host     @0 :import "system.capnp".Host;      # Swarm-level operations (id, addrs, peers, connect).
  executor @1 :import "system.capnp".Executor;  # WASM execution (runBytes, echo).
  ipfs     @2 :import "ipfs.capnp".Client;      # IPFS CoreAPI (unixfs, block, dag, ...).
}


interface Membrane {
  graft @0 (signer :Signer) -> (session :Session);
  # Graft a signer to the membrane, establishing an epoch-scoped session.
}
