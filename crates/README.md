# crates

Rust libraries consumed by the host binary or shared between host and guests.
Nothing in `crates/` ships in the namespace directly.

| Crate | Role |
|-------|------|
| `glia/` | Glia evaluator -- the Lisp interpreter used by the shell and kernel. |
| `membrane/` | Membrane types -- capability hub, epoch lifecycle, provenance. |
| `stem/` | Epoch sources -- StemSource trait for atomic (on-chain) and eventual (IPNS) backends. |
| `atom/` | On-chain Atom -- linearizable register backed by a smart contract. |
| `cache/` | CID cache -- content-addressed storage layer. |
| `schema-id/` | Build helper -- extracts Cap'n Proto schema CIDs at compile time. |
| `guest/auth/` | Shared auth types -- common authentication structures. |

## vs std/

`crates/` = Rust libraries (host-side code, shared types, the Glia evaluator).
`std/` = content that ships in the `ww` namespace (WASM cells, Glia modules, guest SDK).
