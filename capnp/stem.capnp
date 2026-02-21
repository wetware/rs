# Vendored from github.com/wetware/stem.  This file MUST stay in sync with
# stem's canonical copy.  No Rust code is generated for it — build.rs uses
# capnpc::CompilerCommand::crate_provides("stem", [...]) so that downstream
# schemas can import it for type resolution while referencing the stem crate's
# generated types at compile time.  See doc/capnp-cross-crate.md.

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
