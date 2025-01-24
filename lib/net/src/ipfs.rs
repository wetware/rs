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

    pub fn get_file(&self, path: &str) -> BoxStream<Bytes, Error> {
        self.client.cat(path)
    }

    pub async fn ls(&self, path: &str) -> Result<Vec<String>, ipfs_api_backend_hyper::Error> {
        let files = self.client.ls(path).await;
        match files {
            Ok(f) => Ok(f.objects.iter().map(|file| file.hash.to_owned()).collect()),
            Err(e) => Err(e),
        }
    }
}
