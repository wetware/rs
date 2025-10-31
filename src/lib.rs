//! Wetware - P2P sandbox for Web3 applications
//!
//! This library provides cell execution capabilities using Wasmtime, supporting
//! per-stream instantiation with duplex pipe communication.

pub mod cell;
pub mod cli;
pub mod config;
pub mod guest;
pub mod loaders;
pub mod net;

// Re-export commonly used types for convenience
pub use cell::{Config, Loader, Proc};
pub use config::LogLevel;
pub use net::boot::BootConfig;
pub use net::resolver::ServiceResolver;
