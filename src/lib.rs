//! Wetware - P2P sandbox for Web3 applications
//!
//! This library provides cell execution capabilities using Wasmtime, supporting
//! per-stream instantiation with duplex pipe communication.

pub mod cell;
pub mod cli;
pub mod guest;

// Re-export commonly used types for convenience
pub use cell::{Config, Proc};
pub use cli::config::LogLevel;
