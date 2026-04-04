# Mindshare

Symmetric peer-to-peer context sharing for LLMs.

## What it demonstrates

- **Bidirectional capability exchange** -- both peers export a Mindshare to each other, not client-server
- **Sub-capability attenuation** -- Mindshare returns separate ContextWriter and Prompt capabilities, each independently grantable and revocable
- **Rate limiting as capability wrapper** -- token bucket intrinsic to the Prompt object, not an external policy check
- **Content-addressed push** -- context pushed as UnixFS CIDs, resolved from the receiver's local content store
- **Local LLM integration** -- HttpClient POST to Ollama/llama-server, no cloud APIs
- **Schema-keyed DHT discovery** -- peers running Mindshare find each other automatically via schema CID

## How it works

```
Louis                        DHT                        Casey
  |                           |                           |
  |-- find_providers(BD) ---->|                           |
  |<-- [casey, ...] ----------|                           |
  |                           |                           |
  |-- dial + trust ceremony --------------------------->  |
  |<----- exchange Mindshare capabilities --------------->|
  |                           |                           |
  |-- push(notes.md CID) -------------------------------->|
  |                           |        [stored at /louis/] |
  |                           |                           |
  |-- ask("what's the key insight?") -------------------->|
  |<-- "The main finding is..." --------------------------|
  |                           |                           |
  |        [stored at /casey/]|<-- push(reply.md CID) ----|
  |<----------------------------------------------------- |
  |                           |                           |
  |  ... async, bidirectional, long-lived ...              |
```

Both sides push content freely. Both sides prompt freely. Connections survive
disconnects. Each peer's LLM reads across all context pushed into its store.

## Intended use

Cardinality reduction during high-divergence conversations. Two researchers
dump context at each other, and the LLM on each side helps elicit signal
from noise across the shared context surface.

## Prerequisites

- Rust toolchain with `wasm32-wasip2` target:
  ```sh
  rustup target add wasm32-wasip2
  ```
- A local LLM server (e.g. [Ollama](https://ollama.com)) running on `localhost:11434`

## Building

```sh
make mindshare
```

## Schema

`mindshare.capnp` defines:

- **`Mindshare`** -- top-level capability with two sub-capabilities:
  - `context()` -- returns a ContextWriter for pushing content
  - `prompt()` -- returns a rate-limited Prompt for querying the LLM
- **`ContextWriter`** -- single method:
  - `push(cid, meta, inlineContent)` -- push content by CID or inline bytes
- **`Prompt`** -- two methods:
  - `ask(text)` -- prompt the local LLM with all pushed context
  - `remaining()` -- check rate limit (calls left, reset time)
- **`Metadata`** -- optional fields on pushed content:
  - `contentType` -- MIME type hint
  - `tags` -- freeform tags
  - `relation` -- future: supports/contradicts/refines/supersedes
  - `supersedes` -- future: CID of content this replaces

## Security

- **Capability IS access control.** No ACLs, no auth tokens. You hold a Mindshare
  capability or you don't. Revocation = drop the capability.
- **Per-peer isolation.** Each sender writes to a jailed prefix. No cross-contamination.
- **Rate limiting intrinsic to Prompt.** The token bucket travels with the capability.
  Attenuate by wrapping with a smaller bucket. No external rate limiter to bypass.
- **Domain-scoped HTTP.** HttpClient enforces host allowlist. The LLM call stays local.
- **Epoch-guarded.** All capabilities revoke on epoch advance. Peers re-graft automatically.

## Running

### Step 1: Boot the node

Stack the mindshare layer on top of the kernel. The init.d script
registers the Mindshare cell with the host's `VatListener` and grants
it an `HttpClient` capability for local LLM queries.

```sh
ww run --port=2025 crates/kernel examples/mindshare
```

This drops you into a Glia shell.

### Step 2: Start the DHT service

From the Glia shell, run mindshare in service mode to provide the
schema CID on the DHT:

```clojure
/ > (perform runtime :run (load "bin/mindshare.wasm") "serve")
```

### Step 3: Connect a second peer

Open a second terminal:

```sh
ww run --port=2026 crates/kernel examples/mindshare
```

From the second Glia shell, start the service and connect:

```clojure
/ > (perform runtime :run (load "bin/mindshare.wasm") "serve")
```

The two peers will discover each other via DHT and exchange Mindshare
capabilities automatically.

> **Note:** Cell logic is currently a stub. The init.d script and
> schema are ready; the runtime behavior will land in a follow-up PR.

## Init.d script

`etc/init.d/mindshare.glia`:

```clojure
;; Grant an HttpClient capability (for local LLM) and define the cell.
(def mindshare
  (with [(http (perform host :http-client))]
    (cell (load "bin/mindshare.wasm")
          (load "bin/mindshare.schema"))))

(perform host :listen mindshare)
```

**`with`** creates local capability bindings. **`cell`** bundles
wasm + schema + all capabilities from scope. The `HttpClient` is
needed for outbound HTTP calls to the local LLM server (Ollama).

The service mode is started interactively from the Glia shell --
not from the init.d script.

## From the shell

```clojure
;; Connect to a peer
(perform host :connect "/ip4/192.168.1.42/tcp/2025/p2p/12D3Koo...")

;; Get their Mindshare capability
(def bd (perform mindshare :open "12D3Koo..."))

;; Push context — UnixFS paths are first-class
(perform bd :push "/ipfs/QmNotes.../idea.md")
(perform bd :push "/ipfs/QmData.../results.csv" {:tags ["experiment" "v2"]})

;; Prompt against their context
(perform bd :ask "what's the strongest objection to this?")
;; => "The main gap is..."

;; Check rate limit
(perform bd :remaining)
;; => {:calls 8 :reset-in 3600}

;; Read what they've pushed to you
(perform ipfs :cat "/ipfs/QmCasey.../reply.md")
```

## Files

```
examples/mindshare/
├── Cargo.toml
├── Makefile               # make mindshare
├── README.md              # this file
├── mindshare.capnp        # Mindshare schema source
├── bin/                   # build output (gitignored)
│   ├── mindshare.wasm
│   └── mindshare.schema   # compiled schema bytes
├── etc/
│   └── init.d/
│       └── mindshare.glia # cell registration
└── src/
    └── lib.rs             # guest implementation
```

## Roadmap

- **Trust ceremony** -- identity verification before capability export (TBD)
- **Reconnect persistence** -- Mindshare survives disconnection, context persists
- **Context window management** -- summarization/focus when context exceeds LLM limits
- **Epistemic graphs** -- Metadata `relation` and `supersedes` fields enable structured
  reasoning over a DAG of claims (supports, contradicts, refines)
- **Multi-party research rooms** -- N peers, shared LLM surface, capability-scoped views
- **notes.alembic.network** -- Obsidian-style web explorer for mindshare content
