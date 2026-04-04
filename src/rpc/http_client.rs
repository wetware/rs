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

impl EpochGuardedHttpProxy {
    /// Validate epoch, parse URL, and enforce domain scoping.
    fn validate_request(&self, url_str: &str) -> Result<reqwest::Url, capnp::Error> {
        self.guard.check()?;

        let parsed = reqwest::Url::parse(url_str)
            .map_err(|e| capnp::Error::failed(format!("invalid URL: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| capnp::Error::failed("URL has no host".into()))?;

        if !self.allowed_hosts.is_empty() && !self.allowed_hosts.iter().any(|h| h == host) {
            return Err(capnp::Error::failed(format!(
                "host {host:?} not in allowlist"
            )));
        }

        Ok(parsed)
    }

    /// Extract headers from a Cap'n Proto header list into a vec of (name, value) pairs.
    fn extract_headers(
        headers: capnp::struct_list::Reader<'_, http_capnp::header::Owned>,
    ) -> Result<Vec<(String, String)>, capnp::Error> {
        let mut out = Vec::with_capacity(headers.len() as usize);
        for i in 0..headers.len() {
            let h = headers.get(i);
            let name = h
                .get_name()?
                .to_str()
                .map_err(|e| capnp::Error::failed(e.to_string()))?
                .to_string();
            let value = h
                .get_value()?
                .to_str()
                .map_err(|e| capnp::Error::failed(e.to_string()))?
                .to_string();
            out.push((name, value));
        }
        Ok(out)
    }

    /// Execute a request and serialize the response into Cap'n Proto results.
    async fn execute_and_serialize<T>(
        client: reqwest::Client,
        request: reqwest::Request,
        mut results: T,
    ) -> Result<(), capnp::Error>
    where
        T: ResponseBuilder,
    {
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

        results.set_response(status, &resp_headers, &body);
        Ok(())
    }
}

/// Trait to abstract over GetResults and PostResults for response serialization.
trait ResponseBuilder {
    fn set_response(&mut self, status: u16, headers: &[(String, String)], body: &[u8]);
}

impl ResponseBuilder for http_capnp::http_client::GetResults {
    fn set_response(&mut self, status: u16, headers: &[(String, String)], body: &[u8]) {
        let mut res = self.get();
        res.set_status(status);
        res.set_body(body);
        let mut header_list = res.init_headers(headers.len() as u32);
        for (i, (name, value)) in headers.iter().enumerate() {
            let mut h = header_list.reborrow().get(i as u32);
            h.set_name(name);
            h.set_value(value);
        }
    }
}

impl ResponseBuilder for http_capnp::http_client::PostResults {
    fn set_response(&mut self, status: u16, headers: &[(String, String)], body: &[u8]) {
        let mut res = self.get();
        res.set_status(status);
        res.set_body(body);
        let mut header_list = res.init_headers(headers.len() as u32);
        for (i, (name, value)) in headers.iter().enumerate() {
            let mut h = header_list.reborrow().get(i as u32);
            h.set_name(name);
            h.set_value(value);
        }
    }
}

#[allow(refining_impl_trait)]
impl http_capnp::http_client::Server for EpochGuardedHttpProxy {
    fn get(
        self: capnp::capability::Rc<Self>,
        params: http_capnp::http_client::GetParams,
        results: http_capnp::http_client::GetResults,
    ) -> Promise<(), capnp::Error> {
        let reader = pry!(params.get());
        let url_str = pry!(pry!(reader.get_url())
            .to_str()
            .map_err(|e| capnp::Error::failed(e.to_string())));

        pry!(self.validate_request(url_str));

        let req_headers = pry!(Self::extract_headers(pry!(reader.get_headers())));
        let mut builder = self.client.get(url_str);
        for (name, value) in &req_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }

        let request = pry!(builder
            .build()
            .map_err(|e| capnp::Error::failed(format!("failed to build request: {e}"))));
        let client = self.client.clone();

        Promise::from_future(async move {
            Self::execute_and_serialize(client, request, results).await
        })
    }

    fn post(
        self: capnp::capability::Rc<Self>,
        params: http_capnp::http_client::PostParams,
        results: http_capnp::http_client::PostResults,
    ) -> Promise<(), capnp::Error> {
        let reader = pry!(params.get());
        let url_str = pry!(pry!(reader.get_url())
            .to_str()
            .map_err(|e| capnp::Error::failed(e.to_string())));

        pry!(self.validate_request(url_str));

        let req_headers = pry!(Self::extract_headers(pry!(reader.get_headers())));
        let body_bytes = pry!(reader.get_body()).to_vec();

        let mut builder = self.client.post(url_str).body(body_bytes);
        for (name, value) in &req_headers {
            builder = builder.header(name.as_str(), value.as_str());
        }

        let request = pry!(builder
            .build()
            .map_err(|e| capnp::Error::failed(format!("failed to build request: {e}"))));
        let client = self.client.clone();

        Promise::from_future(async move {
            Self::execute_and_serialize(client, request, results).await
        })
    }
}
