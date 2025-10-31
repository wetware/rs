/*! Networking utilities for Wetware

This module groups runtime networking helpers that were previously located under `cli`.
By moving them here, we decouple the `cell` runtime from the `cli` layer and avoid
circular dependencies.

Contents:
- `boot`: Boot peer discovery and DHT bootstrap configuration helpers.
- `resolver`: Service resolver for versioned WASM services and IPFS content.
- `ipfs`: IPFS-related helpers (defaults, HTTP API utilities).
*/

pub mod boot;
pub mod ipfs;
pub mod resolver;
