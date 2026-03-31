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

  executor @3 () -> (executor :Executor);
  # Obtain an Executor scoped to the same epoch as this Host.

  network @4 () -> (streamListener :StreamListener, streamDialer :StreamDialer,
                    vatListener :VatListener, vatClient :VatClient);
  # Obtain StreamListener/StreamDialer (libp2p byte-stream mode) and
  # VatListener/VatClient (Cap'n Proto capability mode) for subprotocol I/O.
}

interface StreamListener {
  listen @0 (executor :Executor, protocol :Text, wasm :Data) -> ();
  # Accept incoming libp2p streams on /ww/0.1.0/stream/{protocol}.
  # For each stream, spawn a cell process via executor.runBytes(wasm)
  # and wire stdin/stdout to the stream.
  #
  # OCAP: caller delegates spawn authority via executor. Wrap executor in an attenuating
  # proxy to restrict cell resources (memory, CPU, network).
}

interface StreamDialer {
  dial @0 (peer :Data, protocol :Text) -> (stream :ByteStream);
  # Open a libp2p stream to peer on /ww/0.1.0/stream/{protocol}.
  # Returns a bidirectional ByteStream: read() pulls from the remote,
  # write() pushes to the remote, close() shuts down both directions.
}

interface Executor {
  runBytes @0 (wasm :Data, args :List(Text), env :List(Text)) -> (process :Process);
  # Instantiate a WASM component from raw bytes and return a handle to
  # its running process.

  echo @1 (message :Text) -> (response :Text);
  # Diagnostic echo — returns the message unmodified.

  bind @2 (wasm :Data, args :List(Text), env :List(Text)) -> (bound :BoundExecutor);
  # Store WASM bytes and bind args/env. Returns a BoundExecutor
  # that can only spawn instances of the bound binary.
  #
  # Capability attenuation: the caller can scale a pool of identical
  # workers but cannot spawn arbitrary code.
}

interface BoundExecutor {
  spawn @0 () -> (process :Process);
  # Spawn a new instance of the bound WASM binary.
  # Each call creates a fresh process with its own stdin/stdout/stderr.
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

  bootstrap @4 () -> (cap :AnyPointer);
  # Return the capability exported by the guest via system::serve().
  # The cap is type-erased — cast to the expected interface on the guest side.
  # Errors if the guest didn't export a capability.

  kill @5 () -> ();
  # Terminate the process immediately. Fuel is revoked and the cell traps.
}

interface VatListener {
  listen @0 (executor :Executor, wasm :Data) -> ();
  # Accept incoming Cap'n Proto RPC connections on /ww/0.1.0/rpc/{cid}
  # where cid = CIDv1(raw, BLAKE3(schema)).
  # The schema is extracted from the WASM binary's "schema.capnp" custom section.
  # The section contains the canonical Cap'n Proto encoding of a schema.Node — its id field
  # (the 64-bit unique type ID) is part of the hash input, so identical structures
  # with different IDs produce different protocol addresses.
  #
  # For each connection, spawn a cell process via executor.runBytes(wasm).
  # The cell calls system::serve() to export a bootstrap capability, which
  # flows back to the connecting peer via Cap'n Proto RPC bootstrapping.
  #
  # The cell's Membrane is never exposed to the remote peer.
  # OCAP: caller delegates spawn authority via executor.
  #
  # Errors if the cell WASM does not contain a "schema.capnp" custom section.
}

interface VatClient {
  dial @0 (peer :Data, schema :Data) -> (cap :AnyPointer);
  # Open a Cap'n Proto RPC connection to peer on /ww/0.1.0/rpc/{cid}
  # where cid = CIDv1(raw, BLAKE3(schema)).
  # The schema is the canonical Cap'n Proto encoding of a schema.Node.
  # Bootstraps a Cap'n Proto vat over the stream and returns the remote
  # cell's bootstrap capability.
  #
  # The returned cap is type-erased (AnyPointer) — cast it to the expected
  # interface type on the guest side.
}

interface ByteStream {
  read @0 (maxBytes :UInt32) -> (data :Data);
  # Read up to maxBytes from the stream.  Returns empty data at EOF.

  write @1 (data :Data) -> ();
  # Write data to the stream.

  close @2 () -> ();
  # Close the stream.  Further reads return EOF; further writes fail.
}
