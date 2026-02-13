@0xa59e04af26eca82f;

using Stem = import "stem.capnp";
using Peer = import "peer.capnp";

struct WetwareSession {
  host     @0 :Peer.Host;
  executor @1 :Peer.Executor;
}

# The bootstrap type for wetware guests is Stem.Membrane(WetwareSession).
# No new interface needed — we use stem's generic Membrane directly.
