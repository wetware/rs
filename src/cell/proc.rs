use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncRead, AsyncWrite};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config as WasmConfig, Engine, Store};
use wasmtime_wasi::cli::{AsyncStdinStream, AsyncStdoutStream};
use wasmtime_wasi::p2::add_to_linker_async;
use wasmtime_wasi::p2::bindings::Command as WasiCliCommand;
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::Loader;

pub const BUFFER_SIZE: usize = 1024;

type BoxAsyncRead = Box<dyn AsyncRead + Send + Sync + Unpin + 'static>;
type BoxAsyncWrite = Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>;

// Required for WASI IO to work.
pub struct ComponentRunStates {
    pub wasi_ctx: WasiCtx,
    pub resource_table: ResourceTable,
    pub loader: Option<Box<dyn Loader>>,
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

/// Builder for constructing a Proc configuration
pub struct Builder {
    env: Vec<String>,
    args: Vec<String>,
    wasm_debug: bool,
    bytecode: Option<Vec<u8>>,
    loader: Option<Box<dyn Loader>>,
    stdin: Option<BoxAsyncRead>,
    stdout: Option<BoxAsyncWrite>,
    stderr: Option<BoxAsyncWrite>,
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
            stdin: None,
            stdout: None,
            stderr: None,
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

    /// Build a Proc instance. All required parameters must be supplied first.
    pub async fn build(self) -> Result<Proc> {
        let Builder {
            env,
            args,
            wasm_debug,
            bytecode,
            loader,
            stdin,
            stdout,
            stderr,
        } = self;

        let bytecode =
            bytecode.ok_or_else(|| anyhow!("bytecode must be provided to Proc::Builder"))?;
        let stdin =
            stdin.ok_or_else(|| anyhow!("stdin handle must be provided to Proc::Builder"))?;
        let stdout =
            stdout.ok_or_else(|| anyhow!("stdout handle must be provided to Proc::Builder"))?;
        let stderr =
            stderr.ok_or_else(|| anyhow!("stderr handle must be provided to Proc::Builder"))?;

        Proc::new(
            env, args, wasm_debug, bytecode, loader, stdin, stdout, stderr,
        )
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
    async fn new(
        env: Vec<String>,
        args: Vec<String>,
        wasm_debug: bool,
        bytecode: Vec<u8>,
        loader: Option<Box<dyn Loader>>,
        stdin: BoxAsyncRead,
        stdout: BoxAsyncWrite,
        stderr: BoxAsyncWrite,
    ) -> Result<Self> {
        let stdin_stream = AsyncStdinStream::new(stdin);
        let stdout_stream = AsyncStdoutStream::new(BUFFER_SIZE, stdout);
        let stderr_stream = AsyncStdoutStream::new(BUFFER_SIZE, stderr);

        let mut wasm_config = WasmConfig::new();
        wasm_config.async_support(true);
        let engine = Engine::new(&wasm_config)?;
        let mut linker = Linker::new(&engine);
        add_to_linker_async(&mut linker)?;

        // Add loader host function if loader is provided
        if loader.is_some() {
            add_loader_to_linker(&mut linker)?;
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

        let state = ComponentRunStates {
            wasi_ctx: wasi,
            resource_table: ResourceTable::new(),
            loader,
        };
        let mut store = Store::new(&engine, state);

        // Instantiate it as a normal component
        let component = Component::from_binary(&engine, &bytecode)?;
        let command = WasiCliCommand::instantiate_async(&mut store, &component, &linker).await?;

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
}
