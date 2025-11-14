@0xf134607b3d5bc69e;

# Router capability for Kademlia DHT operations
# Provides routing functionality to WASM guests

interface Router {
    addPeer @0 (peer :Text, addresses :List(Text)) -> (created :Bool);
    # Add a peer to the Kademlia routing table
    # peer: The peer ID as a string (e.g., "12D3KooW...")
    # addresses: List of multiaddresses for this peer
    # created: Whether the peer was created (true) or updated (false).

    bootstrap @1 (timeout :UInt64) -> (success :Bool);
    # Bootstrap Kademlia DHT participation
    # timeout: Timeout duration in milliseconds
    # This triggers the bootstrap process to discover and connect to peers
}

