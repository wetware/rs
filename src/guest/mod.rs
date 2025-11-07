#![doc = r#"
WASI guest helpers for Cap'n Proto RPC.

This module exposes a single `init` function that wires the WASI CLI
stdin/stdout streams into a Cap'n Proto two-party RPC system. The resulting
`RpcSystem` drives Cap'n Proto clients when polled on an async executor.

Usage (in a WASI guest crate that depends on `ww` with feature `guest` enabled):

```ignore
use ww::guest::{self, rpc_twoparty_capnp, RpcSystem};
use futures::executor::LocalPool;
use futures::future::{join, ready};

fn main() {
    // Initialize RPC system over wasi:cli streams.
    let mut rpc_system: RpcSystem<rpc_twoparty_capnp::Side> = guest::init();
    let _provider_client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);

    // Drive the RPC system on a single-threaded executor. In a real guest
    // program you would poll both the RPC system and your guest logic.
    let mut pool = LocalPool::new();
    pool.run_until(async move {
        let guest = ready(());
        let rpc = async move {
            rpc_system.await;
        };
        let _ = join(guest, rpc).await;
    });
}
```

Notes:
- This module is intended for WASI guests (wasm32 targets) and relies on `wasip2`.
- To compile, enable the `guest` feature in `ww` and target a WASI runtime.
"#]

// The guest implementation depends on WASI Preview 2 APIs (`wasip2`) and Cap'n Proto RPC.
// To avoid pulling these heavy deps into non-guest builds (e.g. native host), the implementation
// is gated behind the "guest" feature. Enable it when compiling a WASI guest that imports `ww::guest`.
#[cfg(feature = "guest")]
pub use capnp_rpc::{rpc_twoparty_capnp, twoparty, RpcSystem};
#[cfg(feature = "guest")]
mod imp {
    use super::{rpc_twoparty_capnp, twoparty, RpcSystem};
    use futures::io::{AsyncRead, AsyncWrite};
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use wasip2::cli::{stdin, stdout};
    use wasip2::io::streams;

    // Use Cap'n Proto RPC primitives re-exported at the crate level.

    /// Initialize a Cap'n Proto RPC system over the WASI CLI stdin/stdout streams.
    ///
    /// This sets up non-blocking adapters around the raw wasi:io streams, wires
    /// them into a two-party VatNetwork, and returns the `RpcSystem`. The local side
    /// is configured as `rpc_twoparty_capnp::Side::Client`; typical guests will
    /// call `RpcSystem::bootstrap` with `rpc_twoparty_capnp::Side::Server` to
    /// obtain the server-provided bootstrap capability.
    ///
    /// The returned `RpcSystem` is a Future that must be driven on an executor
    /// alongside guest logic.
    pub fn init() -> RpcSystem<rpc_twoparty_capnp::Side> {
        init_with_side(rpc_twoparty_capnp::Side::Client)
    }

    /// Same as `init`, but allows specifying the local VAT side explicitly.
    fn init_with_side(side: rpc_twoparty_capnp::Side) -> RpcSystem<rpc_twoparty_capnp::Side> {
        let stdin = Wasip2Stdin::new(stdin::get_stdin());
        let stdout = Wasip2Stdout::new(stdout::get_stdout());

        let network = twoparty::VatNetwork::new(stdin, stdout, side, Default::default());

        RpcSystem::new(Box::new(network), None)
    }

    struct Wasip2Stdin {
        stream: streams::InputStream,
    }

    impl Wasip2Stdin {
        fn new(stream: streams::InputStream) -> Self {
            Self { stream }
        }
    }

    impl AsyncRead for Wasip2Stdin {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            let len = buf.len() as u64;
            match self.stream.read(len) {
                Ok(bytes) => {
                    let n = bytes.len();
                    if n == 0 {
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    buf[..n].copy_from_slice(&bytes);
                    Poll::Ready(Ok(n))
                }
                Err(err) => Poll::Ready(Err(map_stream_error(err))),
            }
        }
    }

    struct Wasip2Stdout {
        stream: streams::OutputStream,
    }

    impl Wasip2Stdout {
        fn new(stream: streams::OutputStream) -> Self {
            Self { stream }
        }
    }

    impl AsyncWrite for Wasip2Stdout {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            if buf.is_empty() {
                return Poll::Ready(Ok(0));
            }

            match self.stream.blocking_write_and_flush(buf) {
                Ok(()) => Poll::Ready(Ok(buf.len())),
                Err(err) => Poll::Ready(Err(map_stream_error(err))),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.stream.blocking_flush() {
                Ok(()) => Poll::Ready(Ok(())),
                Err(err) => Poll::Ready(Err(map_stream_error(err))),
            }
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.stream.blocking_flush() {
                Ok(()) => Poll::Ready(Ok(())),
                Err(err) => Poll::Ready(Err(map_stream_error(err))),
            }
        }
    }

    fn map_stream_error<E: std::fmt::Debug>(err: E) -> io::Error {
        io::Error::other(format!("{err:?}"))
    }
}

// Public re-exports when the guest feature is enabled.
#[cfg(feature = "guest")]
pub use imp::*;
