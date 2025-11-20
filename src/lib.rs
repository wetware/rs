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
pub mod config;
pub mod default_kernel;

// Re-export commonly used types for convenience
#[cfg(not(target_arch = "wasm32"))]
pub use cell::{Loader, Proc, ProcBuilder};
pub use config::LogLevel;
