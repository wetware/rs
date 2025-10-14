//! Wetware - P2P sandbox for Web3 applications
//! 
//! This library provides cell execution capabilities using Wasmtime, supporting
//! per-stream instantiation with duplex pipe communication.

pub mod boot;
pub mod config;
pub mod cell;
pub mod resolver;

// Re-export commonly used types for convenience
pub use cell::{Proc, Config};
pub use config::LogLevel;
