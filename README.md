# Wetware

[![CI](https://github.com/wetware/rs/actions/workflows/rust.yml/badge.svg)](https://github.com/wetware/rs/actions/workflows/rust.yml)

The peer-to-peer agentic OS.

## What is this?

Agentic frameworks give you a platform for running agents. Wetware
gives your agents an **operating system**. It provides primitives
(processes, networking, storage, identity) and gets out of the way.
Processes are network-addressable, capability-secured, and
peer-to-peer by default.

Where agentic frameworks rely on ambient authority — any code can
call any API, read any secret, spend any resource — Wetware replaces
this with capabilities. A process can only do what it's been handed
a capability to do.

## Quick start

```bash
git clone https://github.com/wetware/ww.git && cd ww
make ai-setup      # link config for your AI coding tool
```

Then ask your AI agent:

> Read .agents/prompt.md and get me started with Wetware.

Or browse [`.agents/prompt.md`](.agents/prompt.md) on GitHub — it
works there too.

## How it works

`ww run` boots an agent:

1. Starts a **libp2p swarm** on the configured port
2. Loads `bin/main.wasm` from the merged [image](doc/images.md)
3. Spawns the agent with a **Membrane** — the capability hub that
   grants access to host, network, IPFS, and identity services via
   Cap'n Proto RPC

Agents call `membrane.graft()` to receive epoch-scoped
[capabilities](doc/capabilities.md). When the on-chain epoch
advances (new code deployed, configuration changed), all capabilities
are revoked and the agent must re-graft, picking up the new state
automatically.

## The shell

Glia is a Clojure-inspired language where capabilities are
first-class values. The design blends three traditions:

- **E-lang**: capabilities as values you can pass, compose, and attenuate
- **Clojure**: s-expression syntax, immutable data, functional composition
- **Unix**: processes, PATH lookup, stdin/stdout, init.d scripts

## Building & testing

```bash
rustup target add wasm32-wasip2   # one-time
make                              # build everything (host + std + examples)
cargo test                        # run tests
```

Requires Rust with `wasm32-wasip2` target. Optional:
[Kubo](https://docs.ipfs.tech/install/) for IPFS resolution.

## Container

```bash
make container-build                          # build with podman (default)
CONTAINER_ENGINE=docker make container-build  # or with docker
podman run --rm wetware:latest                # boots kernel + shell
```

## Learn more

- [Architecture](doc/architecture.md) — design principles and capability flow
- [Capabilities](doc/capabilities.md) — the capability model and Cap'n Proto schemas
- [Image layout](doc/images.md) — FHS convention, mounts, and on-chain coordination
- [CLI reference](doc/cli.md) — full command-line usage
- [Shell](doc/shell.md) — Glia shell details
- [Platform vision](doc/designs/economic-agent-platform.md) — roadmap and design
