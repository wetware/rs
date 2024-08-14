use bytes::Bytes;
use ipfs_api_backend_hyper::{Error, IpfsApi, IpfsClient, TryFromUri};
use ipfs_api_prelude::BoxStream;
use libp2p::Multiaddr;

// TODO rename and move to ipfs file
pub struct Client {
    client: IpfsClient,
}

impl Client {
    pub fn new(addr: Multiaddr) -> Self {
        Self {
            client: IpfsClient::from_multiaddr_str(addr.to_string().as_str())
                .expect("error initializing IPFS client"),
        }
    }

    pub fn get_file(&self, path: String) -> BoxStream<Bytes, Error> {
        self.client.cat(path.as_str())
    }
}
