# Braindump

Symmetric peer-to-peer context sharing for LLMs.

## What it demonstrates

- **Bidirectional capability exchange** -- both peers export a Braindump to each other, not client-server
- **Sub-capability attenuation** -- Braindump returns separate ContextWriter and Prompt capabilities, each independently grantable and revocable
- **Rate limiting as capability wrapper** -- token bucket intrinsic to the Prompt object, not an external policy check
- **Content-addressed push** -- context pushed as UnixFS CIDs, resolved from the receiver's local content store
- **Local LLM integration** -- HttpClient POST to Ollama/llama-server, no cloud APIs
- **Schema-keyed DHT discovery** -- peers running Braindump find each other automatically via schema CID

## How it works

```
Louis                        DHT                        Casey
  |                           |                           |
  |-- find_providers(BD) ---->|                           |
  |<-- [casey, ...] ----------|                           |
  |                           |                           |
  |-- dial + trust ceremony --------------------------->  |
  |<----- exchange Braindump capabilities --------------->|
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
make braindump
```

## Schema

`braindump.capnp` defines:

- **`Braindump`** -- top-level capability with two sub-capabilities:
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

- **Capability IS access control.** No ACLs, no auth tokens. You hold a Braindump
  capability or you don't. Revocation = drop the capability.
- **Per-peer isolation.** Each sender writes to a jailed prefix. No cross-contamination.
- **Rate limiting intrinsic to Prompt.** The token bucket travels with the capability.
  Attenuate by wrapping with a smaller bucket. No external rate limiter to bypass.
- **Domain-scoped HTTP.** HttpClient enforces host allowlist. The LLM call stays local.
- **Epoch-guarded.** All capabilities revoke on epoch advance. Peers re-graft automatically.

## From the shell

```clojure
;; Connect to a peer
(perform host :connect "/ip4/192.168.1.42/tcp/2025/p2p/12D3Koo...")

;; Get their Braindump capability
(def bd (perform braindump :open "12D3Koo..."))

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

## Roadmap

- **Trust ceremony** -- identity verification before capability export (TBD)
- **Reconnect persistence** -- Braindump survives disconnection, context persists
- **Context window management** -- summarization/focus when context exceeds LLM limits
- **Epistemic graphs** -- Metadata `relation` and `supersedes` fields enable structured
  reasoning over a DAG of claims (supports, contradicts, refines)
- **Multi-party research rooms** -- N peers, shared LLM surface, capability-scoped views
- **notes.alembic.network** -- Obsidian-style web explorer for braindump content
