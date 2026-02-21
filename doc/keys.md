# Key Management

## Design decisions

### One key, two roles

wetware uses a single **secp256k1** keypair as the node's unified identity.
The same 32-byte private key serves:

1. **libp2p Peer ID** — the `PeerId` derived from the secp256k1 public key
   identifies the node on the p2p network.
2. **EVM / Monad address** — the standard Ethereum address (Keccak256 of the
   uncompressed public key, last 20 bytes) is used for on-chain operations
   against the Stem contract and for Membrane session signing.

This was a deliberate choice against maintaining two separate keys (ed25519 for
p2p, secp256k1 for EVM). One key means:

- **Single source of truth** — your Ethereum address *is* your node identity.
- **Simpler key management** — one file to back up, one file to lose.
- **EVM-native** — secp256k1 is what Monad (and all EVM chains) understand
  natively; ed25519 would require cross-curve bridging.

The performance difference between ed25519 and secp256k1 for libp2p handshakes
is negligible compared to network I/O. libp2p 0.55 supports secp256k1 natively
via the `secp256k1` feature flag.

### Key storage

Keys are stored as raw **lowercase hex** (64 characters = 32 bytes) in a plain
text file. No encryption, no password, no external KMS. The default location is
`~/.ww/key`.

Rationale:
- Simplest possible format — `cat`, `xxd`, `cast`, and Foundry scripts can all
  consume it directly.
- Key files belong on encrypted volumes or in a secrets manager at the
  infrastructure level, not wrapped in application-level encryption that just
  moves the password storage problem.
- Private key material **never touches IPFS** or any other content-addressed
  store. Even if the node CID is public, the key file stays local.

### Ephemeral fallback

If `--key-file` is not provided to `ww run`, an ephemeral secp256k1 key is
generated at startup and discarded on exit. The node uses secp256k1 exclusively
— ed25519 is not used anywhere in the stack. This is fine for local development
and testing but means the node's Peer ID changes on every restart.

## Usage

```sh
# Generate a new key (default: ~/.ww/key)
ww keygen

# Generate to a specific path
ww keygen --output /path/to/key

# Run with a persistent identity
ww run --key-file ~/.ww/key images/my-app
```

## File format

```
# ~/.ww/key — 64 hex characters, no newline required
a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2
```

The `ww keygen` output also prints the derived EVM address and Peer ID:

```
Key written to: /home/user/.ww/key
EVM address:    0x1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b
Peer ID:        12D3KooWAbcDef...
```
