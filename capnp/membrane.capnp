# Wetware session extension for stem's generic Membrane.
#
# The RPC bootstrap capability for wetware guests is
# Stem.Membrane(Session).  Guests call graft() and receive a
# Session whose extension field carries epoch-scoped Host and Executor
# capabilities.  When the on-chain epoch advances, all capabilities from
# the previous session are revoked.

@0xa59e04af26eca82f;

using Stem = import "stem.capnp";
# Vendored copy; no code generated (see crate_provides in build.rs).

using Peer = import "peer.capnp";
# Local peer interfaces: Host, Executor, Process, ByteStream.

using Ipfs = import "ipfs.capnp";
# IPFS CoreAPI-style client capability.

struct Session {
  host     @0 :Peer.Host;      # Swarm-level operations (id, addrs, peers, connect).
  executor @1 :Peer.Executor;  # WASM execution (runBytes, echo).
  ipfs     @2 :Ipfs.Client;    # IPFS CoreAPI (unixfs, block, dag, ...).
}

# The bootstrap type for wetware guests is Stem.Membrane(Session).
# No new interface needed — we reuse stem's generic Membrane directly.
