use std::env;

use libp2p::{identity, kad, Multiaddr};

// Configuration
pub trait Cfg {
    // ID keys uniqely identifying the node.
    fn id_keys(&self) -> identity::Keypair;
    // Name of the protocol used to identify the node through libpb Identify.
    fn identify_protocol(&self) -> String;
    // Multiaddress of the IPFS node.
    fn ipfs_addr(&self) -> Multiaddr;
    // Server or Client. Defaults to server.
    fn kad_mode(&self) -> kad::Mode;
    // Multiaddress the node listens on.
    fn listen_addr(&self) -> Multiaddr;
    // Peer ID of the node. Derived from the public key in id_keys().
    fn peer_id(&self) -> identity::PeerId;
}

// Default node configuration.
pub struct DefaultCfg {
    id_keys: identity::Keypair,
    identify_protocol: String,
    ipfs_addr: Multiaddr,
    listen_addr: Multiaddr,
}

impl DefaultCfg {
    // Default node configuration.
    pub fn new() -> Self {
        Self {
            id_keys: identity::Keypair::generate_ed25519(),
            identify_protocol: "/ww/identify/0.0.1".to_owned(),
            ipfs_addr: "/ip4/127.0.0.1/tcp/5001".to_owned().parse().unwrap(),
            listen_addr: "/ip4/0.0.0.0/tcp/0".to_owned().parse().unwrap(),
        }
    }

    // Check if the node is a Kademlia client from the command-line arguments.
    fn is_kad_client(&self) -> bool {
        let args: Vec<String> = env::args().collect();
        return args.iter().any(|arg| arg == "--kad-client");
    }
}

impl Cfg for DefaultCfg {
    fn identify_protocol(&self) -> String {
        self.identify_protocol.to_owned()
    }

    fn ipfs_addr(&self) -> Multiaddr {
        self.ipfs_addr.to_owned()
    }

    fn listen_addr(&self) -> Multiaddr {
        self.listen_addr.to_owned()
    }

    fn kad_mode(&self) -> kad::Mode {
        if self.is_kad_client() {
            return kad::Mode::Client;
        }
        kad::Mode::Server
    }

    fn id_keys(&self) -> identity::Keypair {
        self.id_keys.clone()
    }

    fn peer_id(&self) -> identity::PeerId {
        identity::PeerId::from(self.id_keys().public())
    }
}
