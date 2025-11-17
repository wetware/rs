# Default Kernel Example

A minimal WASM component that demonstrates the basic setup for a Wetware Protocol guest program.

## Overview

The `default-kernel` example is a minimal implementation that:

- Uses WASI Preview 2 (wasip2) for system interfaces
- Initializes Cap'n Proto RPC over WASI stdin/stdout streams
- Runs a simple event loop to drive the RPC system
- Serves as the default kernel configuration for `ww run`

## RPC Transport Selection

PID0 now requests the optional `wetware:rpc/channel` interface from the host. When the host exposes this interface (via the
new linker wiring in `ww`) the kernel binds Cap'n Proto RPC to the returned `wasi:io/streams` handles, leaving stdio free for
logs. On older hosts, or when the channel is intentionally omitted, the kernel transparently falls back to its original
stdio-based transport.

## Building

### Prerequisites

- **Rust nightly toolchain**: This example requires nightly Rust for `wasm32-wasip2` target support
- **wasm32-wasip2 target**: Install with `rustup target add wasm32-wasip2`

### Build Commands

From the project root:

```bash
# Build via top-level Makefile
make example-default-kernel

# Or build everything (includes this example)
make
```

From this directory:

```bash
# Build directly
make -C examples/default-kernel build

# Or from within this directory
cd examples/default-kernel
make build
```

The build process:
1. Compiles the Rust code to WASM using `wasm32-wasip2` target
2. Copies the generated `.wasm` file to `target/wasm32-wasip2/release/main.wasm`
3. The `main.wasm` file is what `ww run` expects to find

## Running

### Local Filesystem

Run the example using a volume mount:

```bash
./target/release/ww run /boot \
  --volume examples/default-kernel/target/wasm32-wasip2/release:/boot
```

This mounts the build directory to `/boot` inside the WASM environment, where `ww run` will look for `main.wasm`.

### From IPFS

The default kernel can also be run from IPFS. First, rebuild the default config to update the IPFS CID:

```bash
# Rebuild and export to IPFS
make default-config
```

This will:
1. Build the WASM
2. Add it to IPFS as a UnixFS directory
3. Write the CID to `target/default-config.cid`

Then run using the embedded CID:

```bash
# The CID is embedded in the ww binary
./target/release/ww run /ipfs/<CID>
```

Or use the constant from the code:

```rust
use ww::default_kernel::DEFAULT_KERNEL_CID;
// Use DEFAULT_KERNEL_CID to run the default kernel
```

## Code Structure

The example consists of a single entry point in `src/lib.rs`:

```rust
#[no_mangle]
pub extern "C" fn _start() {
    // Initialize RPC system over wasi:cli streams
    let mut rpc_system: RpcSystem<rpc_twoparty_capnp::Side> = guest::init();
    let _provider_client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);

    // Drive the RPC system on a single-threaded executor
    let mut pool = LocalPool::new();
    pool.run_until(async move {
        let guest = ready(());
        let rpc = async move {
            rpc_system.await;
        };
        let _ = join(guest, rpc).await;
    });
}
```

### Key Components

- **`guest::init()`**: Initializes the Cap'n Proto RPC system using WASI CLI streams (stdin/stdout)
- **`rpc_system.bootstrap()`**: Creates a provider client for RPC communication
- **`LocalPool`**: Single-threaded async executor to drive the RPC system
- **`join(guest, rpc)`**: Runs both the guest logic and RPC system concurrently

## Dependencies

The example uses minimal dependencies:

- **`ww`** (with `guest` feature): Provides WASI guest helpers and Cap'n Proto RPC
- **`tokio`** (limited features): Async runtime support (only WASM-compatible features)
- **`futures`**: Async utilities for the event loop

See `Cargo.toml` for exact versions and feature flags.

## Integration with Main Project

The default kernel is integrated into the main `ww` binary:

1. **Build-time**: The IPFS CID is embedded via `build.rs` reading `target/default-config.cid`
2. **Runtime**: The `DEFAULT_KERNEL_CID` constant is available in `ww::default_kernel`
3. **Makefile**: The `default-config` target rebuilds the kernel and updates the CID

To update the default kernel CID:

```bash
make default-config
```

This is automatically run as part of `make all`, ensuring the CID is always up-to-date.

## Troubleshooting

### Build Errors

**Error: `use of unstable library feature 'wasip2'`**
- Ensure you're using Rust nightly: `rustup override set nightly`
- Verify the target is installed: `rustup target add wasm32-wasip2`

**Error: `can't find crate for core`**
- Install the target: `rustup target add wasm32-wasip2 --toolchain nightly`

### Runtime Errors

**Error: `Failed to load main.wasm`**
- Ensure the build completed successfully
- Verify `target/wasm32-wasip2/release/main.wasm` exists
- Check the volume mount path is correct

**Error: IPFS-related errors**
- Ensure IPFS daemon is running: `ipfs daemon`
- Verify the CID in `target/default-config.cid` is valid
- Run `make default-config` to regenerate the CID

## Next Steps

This example provides a minimal foundation. To build more complex guest programs:

1. Add your own RPC service implementations
2. Implement custom guest logic in the event loop
3. Use additional WASI Preview 2 APIs as needed
4. Add more dependencies (ensuring they're WASM-compatible)

See the main project README for more information about the Wetware Protocol and guest development.

