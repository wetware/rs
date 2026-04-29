# Wetware

[![CI](https://github.com/wetware/ww/actions/workflows/rust.yml/badge.svg)](https://github.com/wetware/ww/actions/workflows/rust.yml)

The peer-to-peer OS for autonomous agents.

## Why?

Agentic frameworks give you a platform for running agents. Wetware
gives your agents an **operating system**. It provides primitives
(processes, networking, storage, identity) and gets out of the way.
Processes are network-addressable, capability-secured, and
peer-to-peer by default.

Where agentic frameworks rely on ambient authority -- any code can
call any API, read any secret, spend any resource -- Wetware replaces
this with capabilities. A process can only do what it's been handed
a capability to do. No ambient authority, ever.

## Try it in 60 seconds

```sh
curl -sSL https://wetware.run/install | sh
curl http://localhost:2080/status
```

```json
{
  "status":       "ok",
  "version":      "0.1.0",
  "peer_id":      "12D3KooWRLf8DAFsNfbv3s2DjRMbUuPc8AYdcBfokZbz6kJ2aUss",
  "listen_addrs": ["/ip4/127.0.0.1/tcp/2025", "/ip6/::1/tcp/2025", ...],
  "peer_count":   216
}
```

The second command hit a WebAssembly cell running inside the daemon,
with zero ambient authority. The cell can't read your filesystem,
can't reach the network, can't see your environment variables. The
only thing it can do is what the membrane handed it -- in this case,
the `host` capability, so it can report your peer ID and connected
peers. **The capability is the permission**, there is no blacklist/whitelist.

The wiring lives at `~/.ww/etc/init.d/05-status.glia`. Take a look:

```clojure
(perform host :listen (cell (load "bin/status.wasm")) "/status")
```

That's the whole registration. Read [doc/capabilities.md](doc/capabilities.md)
for the full capability model, and [doc/architecture.md](doc/architecture.md)
for how the membrane graft works.

**Your LLM can do this too.** `ww perform install` wires Wetware
into Claude Code as an MCP server automatically. The same capability
surface curl just hit, an LLM reaches via attenuated capabilities --
same caps, same membrane, same guarantees. See
[.agents/prompt.md](.agents/prompt.md).

## Quick start

```bash
# Install
curl -sSL https://wetware.run/install | sh

# Or build from source
ww doctor                         # check your dev environment
rustup target add wasm32-wasip2   # one-time
make                              # build everything

# Run
ww run .                          # boot a node from current dir
ww shell                          # connect to a local node (auto-discovers via Kubo)
ww shell /dnsaddr/master.wetware.run  # connect to a remote node
```

## How it works

`ww run` boots an agent:

1. Starts a **libp2p swarm** on port 2025
2. Merges [image layers](doc/images.md) into a virtual FHS filesystem
3. Loads `boot/main.wasm` from the merged image
4. Spawns the agent with a **Membrane** -- the capability hub that
   serves named [capabilities](doc/capabilities.md) (Host, Runtime,
   Routing, Identity, HttpClient, and more) over Cap'n Proto RPC

Agents call `membrane.graft()` to receive epoch-scoped capabilities
as a `List(Export)`. When the on-chain epoch advances (new code
deployed, configuration changed), all capabilities are revoked and
the agent must re-graft, picking up the new state automatically.

## Cell modes

WASM processes ("cells") run with zero ambient authority. Their stdio
is wired to a transport based on `WW_CELL_MODE`:

| Mode | stdio carries | Use case |
|------|--------------|----------|
| `vat` | Cap'n Proto RPC | Service mesh, capability exchange |
| `raw` | libp2p stream bytes | Low-level protocols |
| `http` | CGI (WAGI) | HTTP request handlers |
| *(absent)* | Host RPC channel | pid0 kernel -- full membrane graft |

## The shell

Glia is a Clojure-inspired language where capabilities are
first-class values. The design blends three traditions:

- **E-lang**: capabilities as values you can pass, compose, and attenuate
- **Clojure**: s-expression syntax, immutable data, functional composition
- **Unix**: processes, PATH lookup, stdin/stdout, init.d scripts

```
/ > (perform host :id)
"12D3KooWExample..."
/ > (perform host :addrs)
("/ip4/127.0.0.1/tcp/2025" "/ip4/192.168.1.5/tcp/2025")
```

See [doc/shell.md](doc/shell.md) for the full syntax and capability reference.

## AI integration

Wetware is the drivetrain, not the engine. An LLM connects *to* a
node over MCP and gets a Glia shell.

```bash
ww run . --mcp                    # cell as MCP server on stdin/stdout
ww run . --http-listen :2080      # HTTP/WAGI endpoint
```

The `ww perform install` command wires MCP into Claude Code
automatically. See [.agents/prompt.md](.agents/prompt.md) for the
full AI agent reference.

## Standard ports

| Port | Service |
|------|---------|
| 2025 | libp2p swarm |
| 2026 | HTTP admin (metrics, peer ID, listen addrs) |
| 2080 | HTTP/WAGI |

## Building & testing

```bash
ww doctor                         # check dev environment
rustup target add wasm32-wasip2   # one-time
make                              # build everything (host + std + examples)
cargo test                        # run tests
```

Requires Rust with `wasm32-wasip2` target. Optional:
[Kubo](https://docs.ipfs.tech/install/) for IPFS resolution and
peer discovery.

## Container

```bash
make container-build                          # build with podman (default)
CONTAINER_ENGINE=docker make container-build  # or with docker
podman run --rm wetware:latest                # boots kernel + shell
```

## Develop, deploy

```sh
ww init myapp                 # scaffold a new cell project
cd myapp && ww build          # compile to WASM
ww run .                      # test locally
ww push . --ipfs-url http://localhost:5001   # publish to IPFS
ww run /ipfs/<CID>            # run from content-addressed image
```

## Roadmap

The near-term roadmap (see CEO plans in project history):

- **dosync** -- transactional state management for Glia. Atomic
  multi-field updates over content-addressed stems. "Every agent
  gets its own Datomic, as a language primitive."
- **`ww shell` capability discovery** -- attach a shell to a running
  node, enumerate cells, call them via Cap'n Proto from Glia.
- **IPNS hot-reload** -- update a UnixPath to swap init.d scripts;
  capabilities revoke, cells restart, new services appear without
  manual intervention.

## Learn more

- [Architecture](doc/architecture.md) -- design principles and capability flow
- [Capabilities](doc/capabilities.md) -- the capability model and Cap'n Proto schemas
- [CLI reference](doc/cli.md) -- full command-line usage
- [Shell](doc/shell.md) -- Glia shell syntax and capabilities
- [Image layout](doc/images.md) -- FHS convention, mounts, and on-chain coordination
- [Routing](doc/routing.md) -- Kademlia DHT and peer discovery
- [Keys & identity](doc/keys.md) -- Ed25519 identity management
- [RPC transport](doc/rpc-transport.md) -- transport plumbing and scheduling model
- [Guest runtime](doc/guest-runtime.md) -- async runtime for WASM guests
- [Replay protection](doc/replay-protection.md) -- epoch-bound authentication
- [Examples](examples/) -- echo, counter, oracle, chess, and more
