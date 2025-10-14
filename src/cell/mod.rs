//! Cell runtime implementation for Wetware
//! 
//! This module provides cell execution capabilities using Wasmtime, supporting
//! per-stream instantiation with duplex pipe communication.

pub mod proc;
pub mod runtime;
pub mod config;
pub mod executor;

pub use proc::{Proc, Config, ServiceInfo};
pub use executor::{run_cell, Command, WetwareBehaviour};
