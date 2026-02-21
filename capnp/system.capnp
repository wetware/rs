# Wetware peer interfaces.
#
# These capabilities are surfaced to WASM guests through the Membrane's
# epoch-scoped session (see membrane.capnp).  Each capability wrapper
# holds an EpochGuard and fails with a stale-epoch error once the guard
# detects the epoch has advanced.

@0xbf5147b78c0e6a2f;

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

  connect @3 (peerId :Data, addrs :List(Data)) -> ();
  # Dial a peer by ID and multiaddrs.

  executor @4 () -> (executor :Executor);
  # Obtain an Executor scoped to the same epoch as this Host.
}

interface Executor {
  runBytes @0 (wasm :Data, args :List(Text), env :List(Text)) -> (process :Process);
  # Instantiate a WASM component from raw bytes and return a handle to
  # its running process.

  echo @1 (message :Text) -> (response :Text);
  # Diagnostic echo — returns the message unmodified.
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
