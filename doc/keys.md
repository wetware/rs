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

Keys are stored as **base58btc** (Bitcoin alphabet, ~44 characters for 32 bytes)
in a plain text file. Hex-encoded keys are also accepted on load for backward
compatibility. The default location is `~/.ww/key`.

Rationale:
- Denser than hex (44 vs 64 chars), no ambiguous characters (no 0/O/I/l).
- Native to the IPFS ecosystem (same alphabet as CIDv0 and libp2p Peer IDs).
- Key files belong on encrypted volumes or in a secrets manager at the
  infrastructure level, not wrapped in application-level encryption that just
  moves the password storage problem.
- Private key material **never touches IPFS** or any other content-addressed
  store. Even if the node CID is public, the key file stays local.

### Identity resolution

`ww run` resolves the node identity in this order (first match wins):

1. `--identity PATH` — explicit path to a key file
2. `$WW_IDENTITY` — environment variable pointing to a key file
3. `/etc/identity` present in the merged image layers (baked into the image)
4. Ephemeral — generated at startup, discarded on exit

Each time the host resolves the identity it logs the source at `INFO` level
so the active source is always visible in the log output.

The ephemeral fallback is fine for local development and testing but means
the node's Peer ID and EVM address change on every restart. Use a persistent
key for any deployment that other nodes need to remember across restarts.

The Glia config file (`~/.ww/config.glia`) also accepts `:identity` to
set a default key file for daemon mode:

```glia
{:port 2025 :identity "~/.ww/key" :images ["images/my-app"]}
```

## Usage

```sh
# Print a new secret to stdout (metadata on stderr)
ww keygen

# Save to a file
ww keygen --output ~/.ww/key
ww keygen > ~/.ww/key          # equivalent

# Run with a persistent identity
ww run --identity ~/.ww/key images/my-app
```

## File format

```
# ~/.ww/key — base58btc, ~44 chars (hex also accepted on load)
6MRyAjQq8ud7hVNYcfnVPJqcVpscN5So8BhtHuGYqET5
```

`ww keygen` prints the secret to stdout and metadata to stderr:

```
$ ww keygen 2>/dev/null
6MRyAjQq8ud7hVNYcfnVPJqcVpscN5So8BhtHuGYqET5

$ ww keygen --output ~/.ww/key
Secret written to: /home/user/.ww/key
EVM address:    0x1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b
Peer ID:        12D3KooWAbcDef...
```
