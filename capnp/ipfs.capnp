@0xba1bf6f9b41aac9a;

interface UnixFS {
  add @0 (node :Node) -> (cid :Data);
  # Import a file or directory tree into the store and return the root CID.
  #
  # The node is recursive: either a file (leaf bytes) or a directory
  # (named children, each a Node).  The host walks the tree, imports it
  # into the backing store, and returns the root CID.
  #
  # For public stores the CID is content-addressed and pinned.  For
  # private stores the CID is computed locally for integrity only.
  #
  # Mirrors Go CoreAPI UnixfsAPI.Add(ctx, files.Node).

  get @1 (path :Text) -> (node :Node);
  # Retrieve a node by content path.
  #
  # Returns a shallow tree: files carry data, directories carry children
  # as CID references (not full content).  Fetch child contents with
  # subsequent get() calls.
  #
  # For public stores, accepts absolute /ipfs/... paths for global reads.
  # For private stores, paths are relative to the process-local tempdir.
  #
  # Mirrors Go CoreAPI UnixfsAPI.Get(ctx, path).

  ls @2 (path :Text) -> (entries :List(DirEntry));
  # List immediate children of a directory.
  #
  # Convenience: avoids transferring file data for known directories.
  #
  # Mirrors Go CoreAPI UnixfsAPI.Ls(ctx, path).

  struct Node {
    # Capnp projection of Go's files.Node.
    #
    # For add(): callers build the full tree (file data and directory
    # links nested recursively) in a single message.
    #
    # For get(): directories include children as shallow references —
    # Link.node indicates the child's type but may not carry full
    # content.  Use DirEntry.cid from ls() to fetch children.

    union {
      file @0 :Data;
      # Leaf node: raw file content.

      directory @1 :List(Link);
      # Interior node: named links to children.
    }

    struct Link {
      name @0 :Text;
      # Child name within the directory (e.g. "main.wasm").

      node @1 :Node;
      # The child node.
    }
  }

  struct DirEntry {
    name @0 :Text;
    size @1 :UInt64;
    type @2 :Type;
    cid  @3 :Data;

    enum Type { file @0; directory @1; }
  }
}

interface PrivateStore extends(UnixFS) {}
# Process-local ephemeral storage.
#
# Backed by an in-memory map scoped to the WASM process.
# Destroyed on process exit or epoch advance.
# CIDs are computed locally (hash for integrity, not published to IPFS).
# Not content-addressed on the network, not shared, not discoverable.

interface PublicStore extends(UnixFS) {}
# Content-addressed IPFS storage.
#
# Content written here is content-addressed, pinned, and network-retrievable.
# Scoped per-agent: the host prefixes all paths with /ww/<peer-id>/public/
# so multiple `ww run` processes sharing one Kubo node don't collide;
# the guest never sees the prefix.
#
# get() accepts absolute /ipfs/... paths for reading arbitrary network content.
# Root CID from add() is what the agent publishes to the Stem contract.
# Not discoverable by default — peers need the CID via Stem or out-of-band exchange.
