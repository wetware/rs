# Content routing capability backed by Kubo's HTTP API.
#
# Mirrors Go's coreiface.RoutingAPI (provide/findProviders only).
# All methods delegate to Kubo /api/v0/routing/* endpoints; there is no
# in-process Kademlia DHT.
#
# Data transfer (add/cat) lives on the IPFS UnixFS capability, not here.
# DHT key-value store (putValue/getValue) is deferred.
#
# Epoch-scoped: the host wraps the implementation with an EpochGuard so all
# methods fail with stale-epoch once the epoch advances.

@0xa7c3e8f1d4b29065;

struct ProviderInfo {
  peerId @0 :Data;       # libp2p peer ID, serialized.
  addrs  @1 :List(Data); # Multiaddrs for this provider, each serialized.
}

interface ProviderSink {
  provider @0 (info :ProviderInfo) -> stream;
  # Called once per discovered provider.  -> stream enables
  # Cap'n Proto flow control (backpressure).

  done @1 ();
  # Signals that the search is complete.  Errors from earlier
  # provider() calls surface here.
}

interface Routing {
  provide @0 (key :Text) -> ();
  # Announce this node as a provider for the given CID.
  # Kubo POST /api/v0/routing/provide?arg=<key> (experimental).

  findProviders @1 (key :Text, count :UInt32, sink :ProviderSink) -> ();
  # Stream providers for a CID into the caller-supplied sink.
  # Kubo POST /api/v0/routing/findprovs?arg=<key>&num-providers=<count> (stable).
  # Each NDJSON line from Kubo becomes a sink.provider() call.
}
