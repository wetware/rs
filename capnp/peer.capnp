@0xbf5147b78c0e6a2f;

interface Executor {
  runBytes @0 (wasm :Data, args :List(Text), env :List(Text)) -> (process :Process);
  echo @1 (message :Text) -> (response :Text);
}

interface Process {
  stdin @0 () -> (stream :ByteStream);
  stdout @1 () -> (stream :ByteStream);
  stderr @2 () -> (stream :ByteStream);
  wait @3 () -> (exitCode :Int32);
}

interface ByteStream {
  read @0 (maxBytes :UInt32) -> (data :Data);
  write @1 (data :Data) -> ();
  close @2 () -> ();
}
