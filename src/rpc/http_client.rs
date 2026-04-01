//! Epoch-guarded outbound HTTP proxy with domain scoping.
//!
//! The `EpochGuardedHttpProxy` implements the `HttpClient` Cap'n Proto interface,
//! checking the epoch guard and validating the URL host against an allowlist
//! before forwarding the request via `reqwest`.

use capnp::capability::Promise;
use capnp_rpc::pry;
use membrane::EpochGuard;

use crate::http_capnp;

/// Epoch-guarded HTTP proxy that enforces domain scoping.
///
/// If `allowed_hosts` is empty, all hosts are permitted (useful for testing).
/// Otherwise, the URL's host must appear in the allowlist.
pub struct EpochGuardedHttpProxy {
    client: reqwest::Client,
    guard: EpochGuard,
    allowed_hosts: Vec<String>,
}

impl EpochGuardedHttpProxy {
    pub fn new(allowed_hosts: Vec<String>, guard: EpochGuard) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            guard,
            allowed_hosts,
        }
    }
}

#[allow(refining_impl_trait)]
impl http_capnp::http_client::Server for EpochGuardedHttpProxy {
    fn get(
        self: capnp::capability::Rc<Self>,
        params: http_capnp::http_client::GetParams,
        mut results: http_capnp::http_client::GetResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());

        let reader = pry!(params.get());
        let url_str = pry!(pry!(reader.get_url())
            .to_str()
            .map_err(|e| capnp::Error::failed(e.to_string())));

        // Parse the URL and extract host for domain scoping.
        let parsed = pry!(reqwest::Url::parse(url_str)
            .map_err(|e| capnp::Error::failed(format!("invalid URL: {e}"))));
        let host = pry!(parsed
            .host_str()
            .ok_or_else(|| capnp::Error::failed("URL has no host".into())));

        // Domain scoping: if allowed_hosts is non-empty, reject unlisted hosts.
        if !self.allowed_hosts.is_empty() && !self.allowed_hosts.iter().any(|h| h == host) {
            return Promise::err(capnp::Error::failed(format!(
                "host {host:?} not in allowlist"
            )));
        }

        // Collect request headers from the Cap'n Proto message.
        let req_headers = pry!(reader.get_headers());
        let mut builder = self.client.get(url_str);
        for i in 0..req_headers.len() {
            let h = req_headers.get(i);
            let name = pry!(pry!(h.get_name())
                .to_str()
                .map_err(|e| capnp::Error::failed(e.to_string())));
            let value = pry!(pry!(h.get_value())
                .to_str()
                .map_err(|e| capnp::Error::failed(e.to_string())));
            builder = builder.header(name, value);
        }

        let request = pry!(builder
            .build()
            .map_err(|e| capnp::Error::failed(format!("failed to build request: {e}"))));
        let client = self.client.clone();

        Promise::from_future(async move {
            let response = client
                .execute(request)
                .await
                .map_err(|e| capnp::Error::failed(format!("HTTP request failed: {e}")))?;

            let status = response.status().as_u16();
            let resp_headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let body = response
                .bytes()
                .await
                .map_err(|e| capnp::Error::failed(format!("failed to read body: {e}")))?;

            let mut res = results.get();
            res.set_status(status);
            res.set_body(&body);

            let mut header_list = res.init_headers(resp_headers.len() as u32);
            for (i, (name, value)) in resp_headers.iter().enumerate() {
                let mut h = header_list.reborrow().get(i as u32);
                h.set_name(name);
                h.set_value(value);
            }

            Ok(())
        })
    }
}
