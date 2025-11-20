use anyhow::{Context, Result};
use tokio::io::{stderr, stdin, stdout};
use tracing::info;

use crate::cell::{Loader, ProcBuilder};

/// Builder for constructing a cell Command
pub struct CommandBuilder {
    loader: Option<Box<dyn Loader>>,
    path: String,
    args: Vec<String>,
    env: Vec<String>,
    wasm_debug: bool,
    ipfs: Option<crate::ipfs::HttpClient>,
    port: Option<u16>,
    loglvl: Option<crate::config::LogLevel>,
}

impl CommandBuilder {
    /// Create a new Builder with a path
    pub fn new(path: String) -> Self {
        Self {
            loader: None,
            path,
            args: Vec::new(),
            env: Vec::new(),
            wasm_debug: false,
            ipfs: None,
            port: None,
            loglvl: None,
        }
    }

    /// Set the loader
    pub fn with_loader(mut self, loader: Box<dyn Loader>) -> Self {
        self.loader = Some(loader);
        self
    }

    /// Set command line arguments
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Set keyword arguments (environment variables)
    pub fn with_env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }

    /// Set WASM debug mode
    pub fn with_wasm_debug(mut self, wasm_debug: bool) -> Self {
        self.wasm_debug = wasm_debug;
        self
    }

    /// Set the IPFS client
    pub fn with_ipfs(mut self, ipfs: crate::ipfs::HttpClient) -> Self {
        self.ipfs = Some(ipfs);
        self
    }

    /// Set the port
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set the log level
    pub fn with_loglvl(mut self, loglvl: Option<crate::config::LogLevel>) -> Self {
        self.loglvl = loglvl;
        self
    }

    /// Build the Command
    pub fn build(self) -> Command {
        Command {
            path: self.path,
            args: self.args,
            loader: self.loader.expect("loader must be set"),
            ipfs: self.ipfs.expect("ipfs must be set"),
            env: Some(self.env),
            wasm_debug: self.wasm_debug,
            port: self.port.unwrap_or(2020),
            loglvl: self.loglvl,
        }
    }
}

/// Configuration for running a cell
pub struct Command {
    pub path: String,
    pub args: Vec<String>,
    pub loader: Box<dyn Loader>,
    pub ipfs: crate::ipfs::HttpClient,
    pub env: Option<Vec<String>>,
    pub wasm_debug: bool,
    pub port: u16,
    pub loglvl: Option<crate::config::LogLevel>,
}

impl Command {
    /// Execute the cell command
    pub async fn spawn(self) -> Result<i32> {
        let Command {
            path,
            args,
            loader,
            ipfs: _,
            env,
            wasm_debug,
            port: _,
            loglvl,
        } = self;

        let log_level = loglvl.unwrap_or_else(crate::config::get_log_level);
        crate::config::init_tracing(log_level, loglvl);

        info!(binary = %path, "Starting cell execution");

        // Construct the path to main.wasm: <path>/main.wasm
        let wasm_path = format!("{}/main.wasm", path.trim_end_matches('/'));
        let bytecode = loader.load(&wasm_path).await.with_context(|| {
            format!(
                "Failed to load main.wasm from path: {} (resolved to: {})",
                path, wasm_path
            )
        })?;

        let stdin_handle = stdin();
        let stdout_handle = stdout();
        let stderr_handle = stderr();

        ProcBuilder::new()
            .with_wasm_debug(wasm_debug)
            .with_env(env.unwrap_or_default())
            .with_args(args)
            .with_bytecode(bytecode)
            .with_loader(Some(loader))
            .with_stdio(stdin_handle, stdout_handle, stderr_handle)
            .build()
            .await?
            .run()
            .await?;
        info!(binary = %path, "Guest exited");
        Ok(0)
    }
}
