# Wetware peer interfaces.
#
# These capabilities are surfaced to WASM guests through the Membrane's
# epoch-scoped session (see membrane.capnp).  Each capability wrapper
# holds an EpochGuard and fails with a stale-epoch error once the guard
# detects the epoch has advanced.

@0xbf5147b78c0e6a2f;

using Stem = import "stem.capnp";

struct PeerInfo {
  peerId @0 :Data;       # libp2p peer ID, serialized.
  addrs @1 :List(Data);  # Multiaddrs for this peer, each serialized.
}

interface Host {
  id @0 () -> (peerId :Data);
  # Return this node's libp2p peer ID.

  addrs @1 () -> (addrs :List(Data));
  # Return the multiaddrs this node is listening on.

  peers @2 () -> (peers :List(PeerInfo));
  # List currently connected peers.

  executor @3 () -> (executor :Executor);
  # Obtain an Executor scoped to the same epoch as this Host.

  network @4 () -> (listener :Listener, dialer :Dialer);
  # Obtain Listener (accept incoming subprotocol streams) and
  # Dialer (open outgoing subprotocol streams) capabilities.
}

interface Listener {
  listen @0 (executor :Executor, protocol :Text, handler :Data) -> ();
  # Accept incoming streams on /ww/0.1.0/{protocol}. For each stream, spawn a
  # handler process via executor.runBytes(handler) and wire stdin/stdout to the stream.
  #
  # OCAP: caller delegates spawn authority via executor. Wrap executor in an attenuating
  # proxy to restrict handler resources (memory, CPU, network).
}

interface Dialer {
  dial @0 (peer :Data, protocol :Text) -> (stream :ByteStream);
  # Open a stream to peer on /ww/0.1.0/{protocol}.
  # Returns a bidirectional ByteStream: read() pulls from the remote,
  # write() pushes to the remote, close() shuts down both directions.

  dialRpc @1 (peer :Data) -> (terminal :Stem.Terminal(Stem.Membrane));
  # Open /ww/0.1.0 stream to peer, bootstrap Cap'n Proto RPC, and return
  # the peer's Terminal capability for authentication.  After login, the
  # caller receives a Membrane and can graft remote capabilities.
}

interface Executor {
  runBytes @0 (wasm :Data, args :List(Text), env :List(Text)) -> (process :Process);
  # Instantiate a WASM component from raw bytes and return a handle to
  # its running process.

  echo @1 (message :Text) -> (response :Text);
  # Diagnostic echo — returns the message unmodified.

  runCap @2 (wasm :Data, args :List(Text), env :List(Text)) -> (cap :AnyPointer);
  # Like runBytes, but the spawned process exports a bootstrap capability
  # via system::serve().  Returns that capability as AnyPointer — the caller
  # casts it to the expected interface (e.g. ChessEngine).
  # Process lifecycle is tied to the capability: dropping it closes the
  # RPC connection and terminates the guest.
}

interface Process {
  stdin @0 () -> (stream :ByteStream);
  # Writable stream connected to the guest's standard input.

  stdout @1 () -> (stream :ByteStream);
  # Readable stream connected to the guest's standard output.

  stderr @2 () -> (stream :ByteStream);
  # Readable stream connected to the guest's standard error.

  wait @3 () -> (exitCode :Int32);
  # Block until the process exits and return its exit code.
}

interface ByteStream {
  read @0 (maxBytes :UInt32) -> (data :Data);
  # Read up to maxBytes from the stream.  Returns empty data at EOF.

  write @1 (data :Data) -> ();
  # Write data to the stream.

  close @2 () -> ();
  # Close the stream.  Further reads return EOF; further writes fail.
}
