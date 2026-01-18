use anyhow::{anyhow, Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use wasmtime::component::bindgen;
use wasmtime::component::{
    types::ComponentItem, Component, Linker, Resource, ResourceTable, ResourceType,
};
use wasmtime::StoreContextMut;
use wasmtime::{Config as WasmConfig, Engine, Store};
use wasmtime_wasi::cli::{AsyncStdinStream, AsyncStdoutStream};
use wasmtime_wasi::p2::add_to_linker_async;
use wasmtime_wasi::p2::bindings::{Command as WasiCliCommand, CommandPre as WasiCliCommandPre};
use wasmtime_wasi::p2::{InputStream, OutputStream};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::{streams, Loader};

// Generate bindings from WIT file
// Resources are defined within the interface
bindgen!({
    world: "streams-world",
    path: "wit",
});

// Import generated types - Connection is a Resource type alias
use exports::wetware::streams::streams::Connection;

pub const BUFFER_SIZE: usize = 1024;

type BoxAsyncRead = Box<dyn AsyncRead + Send + Sync + Unpin + 'static>;
type BoxAsyncWrite = Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>;

// Note: bindgen! generates a Connection type, but we'll use our own
// to store the stream handles. The generated Connection is the resource type.

// Type aliases for complex channel types
type DataStreamChannel = (
    mpsc::UnboundedSender<Vec<u8>>,   // host_to_guest_tx
    mpsc::UnboundedReceiver<Vec<u8>>, // host_to_guest_rx (for guest input)
    mpsc::UnboundedSender<Vec<u8>>,   // guest_to_host_tx (for guest output)
);

// Required for WASI IO to work.
pub struct ComponentRunStates {
    pub wasi_ctx: WasiCtx,
    pub resource_table: ResourceTable,
    pub loader: Option<Box<dyn Loader>>,
    // Data stream channels for bidirectional host-guest communication
    // Channels needed for creating guest streams (consumed by create-connection)
    // host_to_guest_tx/rx: Host writes -> Guest reads (guest needs input stream)
    // guest_to_host_tx: Guest writes -> Host reads (guest needs output stream)
    pub data_stream_channels: Option<DataStreamChannel>,
}

// Required for WASI IO to work.
impl WasiView for ComponentRunStates {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

// Wrapper for WASI InputStream that we can store as a resource
struct WetwareInputStream {
    inner: Option<Box<dyn InputStream>>,
}

// Wrapper for WASI OutputStream that we can store as a resource
struct WetwareOutputStream {
    inner: Option<Box<dyn OutputStream>>,
}

// Internal connection representation that stores stream wrappers
struct ConnectionState {
    input_stream: Option<WetwareInputStream>,
    output_stream: Option<WetwareOutputStream>,
}

struct ProcInit {
    env: Vec<String>,
    args: Vec<String>,
    wasm_debug: bool,
    bytecode: Vec<u8>,
    loader: Option<Box<dyn Loader>>,
    engine: Option<Arc<Engine>>,
    stdin: BoxAsyncRead,
    stdout: BoxAsyncWrite,
    stderr: BoxAsyncWrite,
    data_streams: Option<DataStreamChannel>,
}

/// Builder for constructing a Proc configuration
pub struct Builder {
    env: Vec<String>,
    args: Vec<String>,
    wasm_debug: bool,
    bytecode: Option<Vec<u8>>,
    loader: Option<Box<dyn Loader>>,
    engine: Option<Arc<Engine>>,
    stdin: Option<BoxAsyncRead>,
    stdout: Option<BoxAsyncWrite>,
    stderr: Option<BoxAsyncWrite>,
    data_streams: Option<DataStreamChannel>,
}

/// Handles for accessing the host-side of data streams.
///
/// These allow the host to read from and write to the data streams
/// that are exposed to the guest via the connection resource.
pub struct DataStreamHandles {
    /// Host can write to this, guest reads from it
    pub host_to_guest_tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Host reads from this, guest writes to it
    guest_to_host_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
}

impl DataStreamHandles {
    /// Move the guest-to-host receiver out so the host can read guest output.
    pub fn take_guest_output_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<u8>>> {
        self.guest_to_host_rx.take()
    }
}

impl Builder {
    /// Create a new Proc builder
    pub fn new() -> Self {
        Self {
            env: Vec::new(),
            args: Vec::new(),
            wasm_debug: false,
            bytecode: None,
            loader: None,
            engine: None,
            stdin: None,
            stdout: None,
            stderr: None,
            data_streams: None,
        }
    }

    /// Set WASM debug mode
    pub fn with_wasm_debug(mut self, debug: bool) -> Self {
        self.wasm_debug = debug;
        self
    }

    /// Add environment variables
    pub fn with_env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }

    /// Add command line arguments
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Provide the component bytecode
    pub fn with_bytecode(mut self, bytecode: Vec<u8>) -> Self {
        self.bytecode = Some(bytecode);
        self
    }

    /// Provide the optional loader used for host callbacks
    pub fn with_loader(mut self, loader: Option<Box<dyn Loader>>) -> Self {
        self.loader = loader;
        self
    }

    /// Provide a shared Wasmtime engine to reuse across processes.
    pub fn with_engine(mut self, engine: Arc<Engine>) -> Self {
        self.engine = Some(engine);
        self
    }

    /// Provide the stdin handle
    pub fn with_stdin<R>(mut self, stdin: R) -> Self
    where
        R: AsyncRead + Send + Sync + Unpin + 'static,
    {
        self.stdin = Some(Box::new(stdin));
        self
    }

    /// Provide the stdout handle
    pub fn with_stdout<W>(mut self, stdout: W) -> Self
    where
        W: AsyncWrite + Send + Sync + Unpin + 'static,
    {
        self.stdout = Some(Box::new(stdout));
        self
    }

    /// Provide the stderr handle
    pub fn with_stderr<W>(mut self, stderr: W) -> Self
    where
        W: AsyncWrite + Send + Sync + Unpin + 'static,
    {
        self.stderr = Some(Box::new(stderr));
        self
    }

    /// Convenience helper to set all stdio handles at once.
    pub fn with_stdio<R, W1, W2>(self, stdin: R, stdout: W1, stderr: W2) -> Self
    where
        R: AsyncRead + Send + Sync + Unpin + 'static,
        W1: AsyncWrite + Send + Sync + Unpin + 'static,
        W2: AsyncWrite + Send + Sync + Unpin + 'static,
    {
        self.with_stdin(stdin)
            .with_stdout(stdout)
            .with_stderr(stderr)
    }

    /// Enable bidirectional data streams for host-guest communication.
    ///
    /// This creates in-memory channels that are exposed to the guest via
    /// a custom connection resource. Returns handles that the host can use
    /// to communicate with the guest.
    pub fn with_data_streams(mut self) -> (Self, DataStreamHandles) {
        let (host_to_guest_tx, host_to_guest_rx, guest_to_host_tx, guest_to_host_rx) =
            streams::create_channel_pair();

        // Clone the sender for the handles (senders are cheap to clone)
        let handles = DataStreamHandles {
            host_to_guest_tx: host_to_guest_tx.clone(),
            guest_to_host_rx: Some(guest_to_host_rx),
        };

        // Store all channels in the builder
        self.data_streams = Some((host_to_guest_tx, host_to_guest_rx, guest_to_host_tx));

        (self, handles)
    }

    /// Build a Proc instance. All required parameters must be supplied first.
    pub async fn build(self) -> Result<Proc> {
        let Builder {
            env,
            args,
            wasm_debug,
            bytecode,
            loader,
            engine,
            stdin,
            stdout,
            stderr,
            data_streams,
        } = self;

        let bytecode =
            bytecode.ok_or_else(|| anyhow!("bytecode must be provided to Proc::Builder"))?;
        let stdin =
            stdin.ok_or_else(|| anyhow!("stdin handle must be provided to Proc::Builder"))?;
        let stdout =
            stdout.ok_or_else(|| anyhow!("stdout handle must be provided to Proc::Builder"))?;
        let stderr =
            stderr.ok_or_else(|| anyhow!("stderr handle must be provided to Proc::Builder"))?;

        Proc::new(ProcInit {
            env,
            args,
            wasm_debug,
            bytecode,
            loader,
            engine,
            stdin,
            stdout,
            stderr,
            data_streams,
        })
        .await
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// Cell process that encapsulates a WASM instance and its configuration.
///
/// Designed for per-stream instantiation - each incoming stream gets its own Proc instance.
/// This enables concurrent execution of multiple services.
pub struct Proc {
    /// Typed handle to the guest command world
    pub command: WasiCliCommand,
    /// Cell runtime store
    #[allow(dead_code)]
    pub store: Store<ComponentRunStates>,
    /// Whether debug info was enabled
    #[allow(dead_code)]
    pub wasm_debug: bool,
}

impl Proc {
    /// Create a new WASM process with explicit stdio handles provided by the host.
    async fn new(init: ProcInit) -> Result<Self> {
        let ProcInit {
            env,
            args,
            wasm_debug,
            bytecode,
            loader,
            engine,
            stdin,
            stdout,
            stderr,
            data_streams,
        } = init;

        let stdin_stream = AsyncStdinStream::new(stdin);
        let stdout_stream = AsyncStdoutStream::new(BUFFER_SIZE, stdout);
        let stderr_stream = AsyncStdoutStream::new(BUFFER_SIZE, stderr);

        let engine = if let Some(engine) = engine {
            engine
        } else {
            let mut wasm_config = WasmConfig::new();
            wasm_config.async_support(true);
            Arc::new(Engine::new(&wasm_config)?)
        };
        let mut linker = Linker::new(&engine);
        tracing::info!("Adding WASI bindings to linker");
        add_to_linker_async(&mut linker)?;
        tracing::info!("WASI bindings added");

        // Add loader host function if loader is provided
        if loader.is_some() {
            tracing::info!("Adding loader bindings to linker");
            add_loader_to_linker(&mut linker)?;
            tracing::info!("Loader bindings added");
        }

        // Prepare environment variables as key-value pairs
        let envs: Vec<(&str, &str)> = env.iter().filter_map(|var| var.split_once('=')).collect();

        // Wire the guest to inherit the host stdio handles.
        let mut wasi_builder = WasiCtxBuilder::new();
        wasi_builder
            .stdin(stdin_stream)
            .stdout(stdout_stream)
            .stderr(stderr_stream)
            .envs(&envs)
            .args(&args);
        let wasi = wasi_builder.build();

        // Set up data streams if enabled
        let data_stream_channels = if let Some(channels) = data_streams {
            let (host_to_guest_tx, host_to_guest_rx, guest_to_host_tx) = channels;
            // Add connection resource and host functions to linker
            tracing::info!("Adding streams bindings to linker");
            add_streams_to_linker(&mut linker)?;
            tracing::info!("Streams bindings added");
            Some((host_to_guest_tx, host_to_guest_rx, guest_to_host_tx))
        } else {
            None
        };

        let state = ComponentRunStates {
            wasi_ctx: wasi,
            resource_table: ResourceTable::new(),
            loader,
            data_stream_channels,
        };

        let mut store = Store::new(&engine, state);

        // Instantiate it as a normal component
        let start = std::time::Instant::now();
        tracing::info!("Compiling guest component");
        let component = Component::from_binary(&engine, &bytecode)?;
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis(),
            "Guest component compiled"
        );
        let component_type = component.component_type();
        tracing::trace!(
            imports = component_type.imports(&engine).len(),
            exports = component_type.exports(&engine).len(),
            "Guest component type summary"
        );
        for (name, item) in component_type.imports(&engine) {
            tracing::trace!(name, item = ?item, "Guest component import");
            if name == "wetware:streams/streams" {
                if let ComponentItem::ComponentInstance(instance) = item {
                    for (export_name, export_item) in instance.exports(&engine) {
                        tracing::trace!(
                            name,
                            export = export_name,
                            item = ?export_item,
                            "Guest streams instance export"
                        );
                    }
                }
            }
        }
        for (name, item) in component_type.exports(&engine) {
            tracing::trace!(name, item = ?item, "Guest component export");
        }

        tracing::info!("Pre-instantiating guest component");
        let pre_start = std::time::Instant::now();
        let pre_instance = linker.instantiate_pre(&component)?;
        let pre = WasiCliCommandPre::new(pre_instance)?;
        tracing::info!(
            elapsed_ms = pre_start.elapsed().as_millis(),
            "Guest component pre-instantiated"
        );

        let start = std::time::Instant::now();
        tracing::info!("Instantiating guest component");
        let command = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pre.instantiate_async(&mut store),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                tracing::error!("Guest component instantiation timed out");
                return Err(anyhow!("guest component instantiation timed out"));
            }
        };
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis(),
            "Guest component instantiated"
        );

        Ok(Self {
            command,
            store,
            wasm_debug,
        })
    }

    /// Invoke the guest's `wasi:cli/run#run` export and wait for completion.
    pub async fn run(mut self) -> Result<()> {
        self.command
            .wasi_cli_run()
            .call_run(&mut self.store)
            .await
            .context("failed to call `wasi:cli/run`")?
            .map_err(|()| anyhow!("guest returned non-zero exit status"))
    }
}

/// Add the streams interface to the Wasmtime linker
///
/// This exports the wetware:streams interface, allowing guests to create
/// connection resources and access bidirectional data streams.
fn add_streams_to_linker(linker: &mut Linker<ComponentRunStates>) -> Result<()> {
    let mut streams_instance = linker.instance("wetware:streams/streams")?;

    // Define the imported connection resource type.
    streams_instance.resource(
        "connection",
        ResourceType::host::<ConnectionState>(),
        |_, _| Ok(()),
    )?;

    // Define the input-stream resource type that wraps WASI InputStream
    streams_instance.resource(
        "input-stream",
        ResourceType::host::<WetwareInputStream>(),
        |_, _| Ok(()),
    )?;

    // Define the output-stream resource type that wraps WASI OutputStream
    streams_instance.resource(
        "output-stream",
        ResourceType::host::<WetwareOutputStream>(),
        |_, _| Ok(()),
    )?;

    // Implement create-connection function.
    streams_instance.func_wrap_async(
        "create-connection",
        |mut store: StoreContextMut<'_, ComponentRunStates>, (): ()| {
            Box::new(async move {
                tracing::info!("streams#create-connection invoked");
                let state = store.data_mut();

                // Extract channels - must exist if this is called
                let channels = state
                    .data_stream_channels
                    .take()
                    .ok_or_else(|| anyhow!("data streams not enabled"))?;

                // Extract only the channels needed for stream creation
                // host_to_guest_tx is kept for potential future use (e.g., host writing to guest)
                let (_host_to_guest_tx, host_to_guest_rx, guest_to_host_tx) = channels;

                // Create stream adapters
                let input_reader = streams::Reader::new(host_to_guest_rx);
                let output_writer = streams::Writer::new(guest_to_host_tx);

                // Wrap in WASI stream types
                let input_stream = AsyncStdinStream::new(Box::new(input_reader));
                let output_stream = AsyncStdoutStream::new(BUFFER_SIZE, Box::new(output_writer));

                // Create wrapper resources for the streams
                let input_wrapper = WetwareInputStream {
                    inner: Some(Box::new(input_stream) as Box<dyn InputStream>),
                };
                let output_wrapper = WetwareOutputStream {
                    inner: Some(Box::new(output_stream) as Box<dyn OutputStream>),
                };

                // Store the stream wrappers in ConnectionState
                let conn_state = ConnectionState {
                    input_stream: Some(input_wrapper),
                    output_stream: Some(output_wrapper),
                };

                // Store in component resource table
                let conn_resource = state.resource_table.push(conn_state)?;

                // Convert Resource<ConnectionState> to Connection (ResourceAny)
                let connection = Connection::try_from_resource(conn_resource, &mut store)?;
                tracing::info!("streams#create-connection returning connection");
                Ok((connection,))
            })
        },
    )?;

    // Implement get-input-stream method.
    streams_instance.func_wrap_async(
        "[method]connection.get-input-stream",
        |mut store: StoreContextMut<'_, ComponentRunStates>,
         (connection,): (Resource<ConnectionState>,)| {
            Box::new(async move {
                tracing::info!("streams#connection.get-input-stream invoked");

                // Take the stream wrapper from ConnectionState
                let stream_wrapper = {
                    let conn_state = store.data_mut().resource_table.get_mut(&connection)?;
                    conn_state
                        .input_stream
                        .take()
                        .ok_or_else(|| anyhow!("input stream already taken"))?
                };

                // Push the wrapper as a resource and return it
                let state = store.data_mut();
                let resource = state.resource_table.push(stream_wrapper)?;
                tracing::info!("streams#connection.get-input-stream returning resource");
                Ok((resource,))
            })
        },
    )?;

    // Implement get-output-stream method.
    streams_instance.func_wrap_async(
        "[method]connection.get-output-stream",
        |mut store: StoreContextMut<'_, ComponentRunStates>,
         (connection,): (Resource<ConnectionState>,)| {
            Box::new(async move {
                tracing::info!("streams#connection.get-output-stream invoked");

                // Take the stream wrapper from ConnectionState
                let stream_wrapper = {
                    let conn_state = store.data_mut().resource_table.get_mut(&connection)?;
                    conn_state
                        .output_stream
                        .take()
                        .ok_or_else(|| anyhow!("output stream already taken"))?
                };

                // Push the wrapper as a resource and return it
                let state = store.data_mut();
                let resource = state.resource_table.push(stream_wrapper)?;
                tracing::info!("streams#connection.get-output-stream returning resource");
                Ok((resource,))
            })
        },
    )?;

    Ok(())
}

/// Add the loader host function to the Wasmtime linker
///
/// This exports a host function that allows WASM guests to call back into
/// the host to load bytecode from various sources (IPFS, filesystem, etc.).
///
/// Note: This requires a WIT interface definition. For now, this is a
/// placeholder that can be implemented once the WIT interface is defined.
fn add_loader_to_linker<T>(_linker: &mut Linker<T>) -> Result<()> {
    // TODO: Implement using WIT interface
    // The WIT interface would look something like:
    //
    // package wetware:loader;
    //
    // interface loader {
    //   load: func(path: string) -> result<list<u8>, string>;
    // }
    //
    // world wetware {
    //   import loader: self.loader;
    // }
    //
    // Then we'd use wit-bindgen to generate bindings and implement:
    // linker.root().func_wrap_async("wetware:loader/loader", "load", |mut store, (path,): (String,)| async move {
    //     let state = store.data_mut();
    //     if let Some(ref loader) = state.loader {
    //         match loader.load(&path).await {
    //             Ok(data) => Ok((data,)),
    //             Err(e) => Err(e.to_string()),
    //         }
    //     } else {
    //         Err("Loader not available".to_string())
    //     }
    // })?;

    // For now, this is a no-op placeholder
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn test_proc_builder_creation() {
        let builder = Builder::new();
        assert!(!builder.wasm_debug);
        assert!(builder.env.is_empty());
        assert!(builder.args.is_empty());
    }

    #[test]
    fn test_proc_builder() {
        let builder = Builder::new()
            .with_wasm_debug(true)
            .with_env(vec!["TEST=1".to_string()])
            .with_args(vec!["arg1".to_string()]);

        assert!(builder.wasm_debug);
        assert_eq!(builder.env.len(), 1);
        assert_eq!(builder.args.len(), 1);
    }

    #[tokio::test]
    async fn test_data_stream_handles_full_duplex() {
        // Enable data streams and capture the returned handles
        let (mut builder, mut handles) = Builder::new().with_data_streams();

        // Access the underlying channels to simulate guest behavior
        let (_host_to_guest_tx_internal, host_to_guest_rx, guest_to_host_tx) = builder
            .data_streams
            .take()
            .expect("data streams should be configured");

        // Host -> guest: send through the handle and ensure the guest side receives it
        handles.host_to_guest_tx.send(b"ping".to_vec()).unwrap();
        let mut guest_reader = streams::Reader::new(host_to_guest_rx);
        let mut buf = vec![0u8; 4];
        let n = guest_reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");

        // Guest -> host: simulate guest write and ensure host can receive via the handle
        let mut guest_output_rx = handles
            .take_guest_output_receiver()
            .expect("guest output receiver should be returned to the host");
        guest_to_host_tx.send(b"pong".to_vec()).unwrap();
        let pong = guest_output_rx
            .recv()
            .await
            .expect("host should receive guest output");
        assert_eq!(pong, b"pong");
    }
}
