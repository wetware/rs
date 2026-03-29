# Capabilities

After grafting onto the Membrane, an agent holds references to:

| Capability | What it does |
|------------|-------------|
| **Host** | Peer identity, listen addresses, connected peers, network access |
| **Executor** | Spawn child WASM processes |
| **IPFS** | Content-addressed storage (cat, ls, add) |
| **Routing** | Kademlia DHT: publish and discover content/services |
| **Identity** | Host-side signing (private key never enters WASM) |
| **StreamListener / StreamDialer** | Open and accept libp2p byte streams for custom subprotocols |
| **VatListener / VatClient** | Serve and consume Cap'n Proto RPC capabilities over the network |

Each capability is epoch-guarded: it fails with `staleEpoch` once the
on-chain head advances, forcing a re-graft.

## Capability lifecycle

1. Agent calls `membrane.graft()` to receive epoch-scoped capabilities
2. Having a Membrane reference IS authorization (ocap model)
3. To gate access, wrap the Membrane in a `Terminal(Membrane)` challenge-response auth layer
4. When the on-chain epoch advances, all capabilities are revoked
5. Agents re-graft, picking up the new state automatically

## Cap'n Proto schemas

Schema definitions live in `capnp/`:

- **`system.capnp`** — Host, Executor, Process, ByteStream, StreamListener, StreamDialer, VatListener, VatClient
- **`stem.capnp`** — Terminal, Membrane, Epoch, Signer, Identity
- **`ipfs.capnp`** — IPFS CoreAPI (UnixFS, Block, Pin, ...)
- **`routing.capnp`** — Kademlia DHT (provide, findProviders, hash)
