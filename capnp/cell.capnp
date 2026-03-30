# Cell type tag for WASM guest modules.
#
# Stored in a WASM custom section named "cell.capnp". The variant tells the
# host what kind of plumbing to wire up:
#
#   raw   — stdin/stdout = raw libp2p stream bytes
#   http  — stdin/stdout = FastCGI records, host wires to HTTP port
#   capnp — stdin/stdout = capnp-rpc, Schema.Node keys the protocol CID
#
# Absent section (no custom section at all) implies pid0 / WIT mode.

@0xd4fccb9cfdc7af3b;

using Schema = import "/capnp/schema.capnp";

struct Cell {
  union {
    raw @0 :Text;              # libp2p protocol ID, e.g. "/ipfs/bitswap/1.2.0"
    http @1 :Text;             # HTTP path prefix, e.g. "/api/v1"
    capnp @2 :Schema.Node;    # typed capability, CID derived from canonical schema
  }
}
