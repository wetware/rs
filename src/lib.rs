//! Wetware - P2P sandbox for Web3 applications
//!
//! This library provides cell execution capabilities using Wasmtime, supporting
//! per-stream instantiation with duplex pipe communication.

// Host-only modules (not available for WASM guests)
#[cfg(not(target_arch = "wasm32"))]
pub mod cell;
#[cfg(not(target_arch = "wasm32"))]
pub mod ipfs;
#[cfg(not(target_arch = "wasm32"))]
pub mod loaders;

// Modules available for both host and guest
pub mod default_kernel;
pub mod config;
pub mod guest;

// Cap'n Proto generated code
// Note: For guests, the schema needs to be compiled separately or included differently
// For now, we'll make it available to both host and guest
pub mod router_capnp {
    include!(concat!(env!("OUT_DIR"), "/src/schema/router_capnp.rs"));
}

// Re-export commonly used types for convenience
#[cfg(not(target_arch = "wasm32"))]
pub use cell::{Loader, ProcBuilder, Proc};
pub use config::LogLevel;
