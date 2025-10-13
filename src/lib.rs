//! Wetware - P2P sandbox for Web3 applications
//! 
//! This library provides cell execution capabilities using Wasmtime, supporting
//! both sync and async modes with method-based routing for cell exports.

pub mod boot;
pub mod config;
pub mod system;
pub mod cell;

// Re-export commonly used types for convenience
pub use cell::{Proc, Config};
pub use config::{LogLevel, HostConfig, AppConfig};
