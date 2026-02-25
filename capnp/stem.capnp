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

interface Membrane {
  graft @0 (signer :Signer) -> (
    identity :Identity,                        # Host-side identity hub: maps signing domains → Signers.
    host     :import "system.capnp".Host,      # Swarm-level operations (id, addrs, peers, connect).
    executor :import "system.capnp".Executor,  # WASM execution (runBytes, echo).
    ipfs     :import "ipfs.capnp".Client       # IPFS CoreAPI (unixfs, block, dag, ...).
  );
  # Graft a signer to the membrane, returning epoch-scoped capabilities.
}
