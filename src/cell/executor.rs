use anyhow::{Context, Result};
use futures::{future, StreamExt};
use libp2p::swarm::SwarmEvent;
use libp2p::{identity, Multiaddr, SwarmBuilder};
use std::time::Duration;
use tracing::{trace, debug, info, warn};

use crate::cell::{Loader, ProcBuilder, Proc, ServiceInfo};
#[cfg(not(target_arch = "wasm32"))]
use crate::cell::router::{RouterCapability, KadOperation};
#[cfg(not(target_arch = "wasm32"))]
use crate::router_capnp;
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::mpsc;

/// Network behavior for Wetware cells
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct WetwareBehaviour {
    pub kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    pub identify: libp2p::identify::Behaviour,
}

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
    pub async fn spawn(self) -> Result<()> {
        // Initialize tracing with the determined log level
        let log_level = self.loglvl.unwrap_or_else(crate::config::get_log_level);
        crate::config::init_tracing(log_level, self.loglvl);

        info!(binary = %self.path, "Starting cell execution");

        let proc_config = ProcBuilder::new()
            .with_wasm_debug(self.wasm_debug)
            .with_env(self.env.clone().unwrap_or_default())
            .with_args(self.args.clone())
            .build();

        // Construct the path to main.wasm: <path>/main.wasm
        let wasm_path = format!("{}/main.wasm", self.path.trim_end_matches('/'));
        let bytecode = self.loader
            .load(&wasm_path)
            .await
            .with_context(|| format!("Failed to load main.wasm from path: {} (resolved to: {})", self.path, wasm_path))?;

        let service_info = ServiceInfo {
            service_path: self.path.to_string(),
            version: "0.1.0".to_string(),
            service_name: "main".to_string(),
            protocol: String::new(),
        };

        // Extract fields needed before moving self.loader
        let loader = self.loader;
        let port = self.port;
        
        let proc = Proc::new_with_duplex_pipes(proc_config, &bytecode, service_info, Some(loader))
            .await
            .with_context(|| format!("Failed to create cell process from binary: {}", self.path))?;

        Self::exec_loop(proc, port).await
    }
    
    /// Run the cell asynchronously with libp2p networking
    async fn exec_loop(proc: Proc, port: u16) -> Result<()> {
        let keypair = identity::Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let kad = libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id));

        let behaviour = WetwareBehaviour {
            kad,
            identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "wetware/0.1.0".to_string(),
                keypair.public(),
            )),
        };

        let mut swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                Default::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )?
            .with_behaviour(|_| behaviour)?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        let (operation_tx, mut operation_rx) = mpsc::channel(100);
        let router_cap = RouterCapability::new(operation_tx);
        let router_client: router_capnp::router::Client = capnp_rpc::new_client(router_cap);
        // Access the inner capability client (typed clients wrap capnp::capability::Client)
        let bootstrap_client: capnp::capability::Client = router_client.client;

        // Create host RPC system
        let host_stdin = proc.host_stdin;
        let host_stdout = proc.host_stdout;
        let rpc_system = Proc::create_host_rpc_system(host_stdin, host_stdout, bootstrap_client);

        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", port).parse()?;
        swarm.listen_on(listen_addr)?;

        info!(peer_id = %peer_id, port = port, "Cell process started");

        // CANONICAL BEHAVIOR: _start was already called during instantiation for initialization
        // Now we start the libp2p event loop to handle incoming streams
        info!(
            "Cell initialized and ready for connections. Incoming streams will be routed to services."
        );

        // Run swarm event loop and RPC system concurrently
        // KAD operations are processed within the swarm event loop
        future::try_join(
            async {
                loop {
                    tokio::select! {
                        event = swarm.select_next_some() => {
                            match event {
                                SwarmEvent::NewListenAddr { address, .. } => {
                                    info!(address = %address, peer_id = %peer_id, "Listening on address");
                                }
                                SwarmEvent::IncomingConnection { .. } => {
                                    debug!("Incoming connection");
                                }
                                SwarmEvent::ConnectionEstablished {
                                    peer_id: remote_peer,
                                    ..
                                } => {
                                    info!(peer_id = %remote_peer, "Connection established");
                                }
                                SwarmEvent::ConnectionClosed {
                                    peer_id: remote_peer,
                                    ..
                                } => {
                                    debug!(peer_id = %remote_peer, "Connection closed");
                                }
                                SwarmEvent::IncomingConnectionError { error, .. } => {
                                    warn!(error = ?error, "Incoming connection error");
                                }

                                // Wetware protocol behaviour.
                                SwarmEvent::Behaviour(event) => {
                                    match event {
                                        WetwareBehaviourEvent::Kad(event) => {
                                            debug!("Kademlia event: {:?}", event);
                                        }
                                        WetwareBehaviourEvent::Identify(event) => {
                                            debug!("Identify event: {:?}", event);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        operation = operation_rx.recv() => {
                            if let Some(op) = operation {
                                match op {
                                    KadOperation::AddPeer { peer_id, addresses, response } => {
                                        for address in addresses {
                                            swarm.behaviour_mut().kad.add_address(&peer_id, address);
                                        }
                                        let _ = response.send(Ok(())).await;
                                    }
                                    KadOperation::Bootstrap { response } => {
                                        match swarm.behaviour_mut().kad.bootstrap() {
                                            Ok(query_id) => {
                                                trace!("Bootstrap query ID: {:?}", query_id);
                                                let _ = response.send(Ok(())).await;
                                            }
                                            Err(e) => {
                                                let _ = response.send(Err(e.to_string())).await;
                                            }
                                        }
                                    }
                                }
                            } else {
                                // Channel closed, exit loop
                                break;
                            }
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            },
            async {
                rpc_system.await?;
                Ok::<(), anyhow::Error>(())
            },
        )
        .await?;

        Ok(())
    }
}





