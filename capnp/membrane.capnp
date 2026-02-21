# Wetware session extension for stem's generic Membrane.
#
# The RPC bootstrap capability for wetware guests is
# Stem.Membrane(Session).  Guests call graft() and receive a
# Session whose extension field carries epoch-scoped Host and Executor
# capabilities.  When the on-chain epoch advances, all capabilities from
# the previous session are revoked.

@0xa59e04af26eca82f;

struct Session {
  host     @0 :import "peer.capnp".Host;      # Swarm-level operations (id, addrs, peers, connect).
  executor @1 :import "peer.capnp".Executor;  # WASM execution (runBytes, echo).
  ipfs     @2 :import "ipfs.capnp".Client;    # IPFS CoreAPI (unixfs, block, dag, ...).
}
