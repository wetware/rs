//! Cap'n Proto RPC tests over WASI streams
//!
//! These tests demonstrate full-duplex, asynchronous RPC communication
//! between host and guest using Cap'n Proto over WASI Preview 2 streams.

use crate::cell::{proc::DataStreamHandles, Loader, ProcBuilder};
use crate::loaders::HostPathLoader;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};

// Cap'n Proto generated code (will be generated when building the guest)
// For now, we'll need to build the guest first to generate this
// mod echo_capnp {
//     include!(concat!(env!("OUT_DIR"), "/echo_capnp.rs"));
// }

// Cap'n Proto will be added as a dependency
// For now, we'll structure the tests to show what we expect

/// Test basic RPC echo functionality
#[tokio::test]
#[ignore] // Ignore until implementation is complete
async fn test_capnp_echo_basic() -> Result<()> {
    // Build guest component path
    let guest_path = PathBuf::from("examples/default-kernel/target/wasm32-wasip2/release");
    let wasm_path = guest_path.join("main.wasm");
    
    // Load guest bytecode
    let loader: Box<dyn Loader> = Box::new(HostPathLoader);
    let bytecode = loader.load(wasm_path.to_str().unwrap()).await
        .context("Failed to load guest WASM")?;
    
    // Create proc with data streams enabled
    let (builder, mut handles) = ProcBuilder::new()
        .with_bytecode(bytecode)
        .with_loader(Some(loader))
        .with_stdio(
            tokio::io::empty(),
            tokio::io::sink(),
            tokio::io::sink(),
        )
        .with_data_streams();
    
    let proc = builder.build().await?;
    
    // Get stream handles for Cap'n Proto client
    let mut guest_output_rx = handles
        .take_guest_output_receiver()
        .expect("guest output receiver should be available");
    
    // Create async read/write wrappers for the channels
    // Host writes to host_to_guest_tx (guest reads from input stream)
    // Host reads from guest_to_host_rx (guest writes to output stream)
    use crate::cell::streams;
    let mut input_stream = streams::Reader::new(guest_output_rx);
    let mut output_stream = streams::Writer::new(handles.host_to_guest_tx);
    
    // Spawn guest in background
    let guest_handle = tokio::spawn(async move {
        proc.run().await
    });
    
    // TODO: Implement Cap'n Proto client
    // For now, this is a placeholder - full RPC client implementation requires
    // proper Cap'n Proto RPC message handling
    // Basic approach:
    // 1. Create Cap'n Proto message with Echo request
    // 2. Write message to input_stream using capnp-futures
    // 3. Read response from output_stream
    // 4. Parse and verify response
    
    // Wait a bit for guest to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // Wait for guest to complete
    guest_handle.await??;
    
    Ok(())
}

/// Test concurrent RPC requests to demonstrate asynchrony
#[tokio::test]
#[ignore] // Ignore until implementation is complete
async fn test_capnp_echo_concurrent() -> Result<()> {
    // Build guest component path
    let guest_path = PathBuf::from("examples/default-kernel/target/wasm32-wasip2/release");
    let wasm_path = guest_path.join("main.wasm");
    
    // Load guest bytecode
    let loader: Box<dyn Loader> = Box::new(HostPathLoader);
    let bytecode = loader.load(wasm_path.to_str().unwrap()).await
        .context("Failed to load guest WASM")?;
    
    // Create proc with data streams enabled
    let (builder, mut handles) = ProcBuilder::new()
        .with_bytecode(bytecode)
        .with_loader(Some(loader))
        .with_stdio(
            tokio::io::empty(),
            tokio::io::sink(),
            tokio::io::sink(),
        )
        .with_data_streams();
    
    let proc = builder.build().await?;
    
    // Get stream handles
    let mut guest_output_rx = handles
        .take_guest_output_receiver()
        .expect("guest output receiver should be available");
    
    // Spawn guest in background
    let guest_handle = tokio::spawn(async move {
        proc.run().await
    });
    
    // TODO: Implement concurrent Cap'n Proto client calls
    // This test should send multiple Echo requests in parallel and verify all responses
    
    // Wait a bit for guest to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // Wait for guest to complete
    guest_handle.await??;
    
    Ok(())
}

/// Test full-duplex communication
#[tokio::test]
#[ignore] // Ignore until implementation is complete
async fn test_capnp_echo_full_duplex() -> Result<()> {
    // Build guest component path
    let guest_path = PathBuf::from("examples/default-kernel/target/wasm32-wasip2/release");
    let wasm_path = guest_path.join("main.wasm");
    
    // Load guest bytecode
    let loader: Box<dyn Loader> = Box::new(HostPathLoader);
    let bytecode = loader.load(wasm_path.to_str().unwrap()).await
        .context("Failed to load guest WASM")?;
    
    // Create proc with data streams enabled
    let (builder, mut handles) = ProcBuilder::new()
        .with_bytecode(bytecode)
        .with_loader(Some(loader))
        .with_stdio(
            tokio::io::empty(),
            tokio::io::sink(),
            tokio::io::sink(),
        )
        .with_data_streams();
    
    let proc = builder.build().await?;
    
    // Get stream handles
    let mut guest_output_rx = handles
        .take_guest_output_receiver()
        .expect("guest output receiver should be available");
    
    // Spawn guest in background
    let guest_handle = tokio::spawn(async move {
        proc.run().await
    });
    
    // TODO: Implement full-duplex Cap'n Proto communication
    // This test should demonstrate bidirectional communication while processing
    
    // Wait a bit for guest to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // Wait for guest to complete
    guest_handle.await??;
    
    Ok(())
}
