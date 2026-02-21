//! Wetware - P2P sandbox for Web3 applications
//!
//! This library provides cell execution capabilities using Wasmtime, supporting
//! per-stream instantiation with duplex pipe communication.

// Host-only modules (not available for WASM guests)
#[cfg(not(target_arch = "wasm32"))]
pub mod cell;
#[cfg(not(target_arch = "wasm32"))]
pub mod epoch;
#[cfg(not(target_arch = "wasm32"))]
pub mod host;
#[cfg(not(target_arch = "wasm32"))]
pub mod image;
#[cfg(not(target_arch = "wasm32"))]
pub mod ipfs;
#[cfg(not(target_arch = "wasm32"))]
pub mod loaders;
#[cfg(not(target_arch = "wasm32"))]
pub mod rpc;

// Re-export capnp schema modules from the membrane crate so host code can
// use `crate::system_capnp`, `crate::ipfs_capnp`, `crate::stem_capnp`.
#[cfg(not(target_arch = "wasm32"))]
pub use membrane::ipfs_capnp;
#[cfg(not(target_arch = "wasm32"))]
pub use membrane::stem_capnp;
#[cfg(not(target_arch = "wasm32"))]
pub use membrane::system_capnp;

// Modules available for both host and guest
pub mod config;
pub mod default_kernel;

// Re-export commonly used types for convenience
#[cfg(not(target_arch = "wasm32"))]
pub use cell::{Loader, Proc, ProcBuilder};
