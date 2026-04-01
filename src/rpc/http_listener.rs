//! HttpListener capability: WAGI/CGI cells served over HTTP.
//!
//! The `HttpListener` capability lets a guest register an HTTP endpoint.
//! For each incoming request matching the path prefix, the host spawns a
//! cell process (via the guest-provided `BoundExecutor`) with CGI env vars
//! as environment, request body piped to stdin, and CGI response read from stdout.
//!
//! **Not yet implemented.** The real implementation requires the WagiService
//! (planned for Phase 1.75). This stub compiles and returns an unimplemented error.

use capnp::capability::Promise;
use membrane::EpochGuard;

use crate::system_capnp;

pub struct HttpListenerImpl {
    #[allow(dead_code)] // used when WagiService is implemented
    guard: EpochGuard,
}

impl HttpListenerImpl {
    pub fn new(guard: EpochGuard) -> Self {
        Self { guard }
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::http_listener::Server for HttpListenerImpl {
    fn listen(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::http_listener::ListenParams,
        _results: system_capnp::http_listener::ListenResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "HttpListener not yet implemented (requires WagiService)".into(),
        ))
    }
}
