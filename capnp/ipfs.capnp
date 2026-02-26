# IPFS capability interfaces, modeled after Go's CoreAPI.
#
# The Client interface mirrors github.com/ipfs/kubo/core/coreiface.CoreAPI,
# providing sub-API accessors for UnixFS, Block, DAG, etc.
#
# Only UnixFS is implemented initially; other sub-APIs are stubs.

@0xba1bf6f9b41aac9a;

interface Client {
  unixfs      @0  () -> (api :UnixFS);
  block       @1  () -> (api :Block);
  dag         @2  () -> (api :Dag);
  name        @3  () -> (api :Name);
  key         @4  () -> (api :Key);
  pin         @5  () -> (api :Pin);
  object      @6  () -> (api :Object);
  swarm       @7  () -> (api :Swarm);
  pubSub      @8  () -> (api :PubSub);
  routing     @9  () -> (api :Routing);
  resolvePath @10 (path :Text) -> (resolved :Text, remainder :List(Text));
  resolveNode @11 (path :Text) -> (node :Data);
}

interface UnixFS {
  cat @0 (path :Text) -> (data :Data);
  # Fetch content at the given IPFS path.
  # Kubo POST /api/v0/cat?arg=<path> (stable).

  ls  @1 (path :Text) -> (entries :List(Entry));
  # List directory entries at the given IPFS path.
  # Kubo POST /api/v0/ls?arg=<path> (stable).

  add @2 (data :Data) -> (cid :Text);
  # Store data in IPFS, returning the content-addressed CID.
  # Kubo POST /api/v0/add (stable).

  struct Entry {
    name @0 :Text;
    size @1 :UInt64;
    type @2 :EntryType;
    cid  @3 :Data;

    enum EntryType { file @0; directory @1; }
  }
}

# Stub interfaces — filled in as needed.
interface Block {}
interface Dag {}
interface Name {}
interface Key {}
interface Pin {}
interface Object {}
interface Swarm {}
interface PubSub {}
interface Routing {}
