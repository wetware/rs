//! Cell runtime implementation for Wetware
//! 
//! This module provides cell execution capabilities using Wasmtime, supporting
//! both sync and async modes with method-based routing for cell exports.

pub mod endpoint;
pub mod proc;
pub mod runtime;
pub mod config;
pub mod stream;
pub mod pipe;

pub use proc::{Proc, Config};
pub use runtime::WasmRuntime;
