# Virtual filesystem capability.
#
# Provides scoped access to the WASI virtual filesystem.
# The host preopens the merged FHS image directory at `/`.
#
# This interface exists for schema identity (CID) purposes.
# It is not served over Cap'n Proto RPC — the kernel handles
# it locally via WASI filesystem calls.

@0xe8a3f1c2d4b5a697;

interface Fs {
  read @0 (path :Text) -> (data :Data);
  # Read the entire contents of a file.
  # Relative paths resolve against the WASI root (/).
}
