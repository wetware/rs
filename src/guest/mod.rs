#![doc = r#"
WASI guest helpers for Cap'n Proto RPC.

This module exposes helpers for wiring a host-provided full-duplex transport
into a Cap'n Proto two-party RPC system. Guests are expected to request a
dedicated channel (e.g. `wetware:rpc/channel`) and feed it to `init_with_stream`.

Usage (in a WASI guest crate that depends on `ww` with feature `guest` enabled):

```ignore
use ww::guest::{self, rpc_twoparty_capnp, DuplexStream, RpcSystem};
use futures::executor::LocalPool;
use futures::future::{join, ready};

fn main() {
    // Acquire the dedicated transport that the host exposes.
    let stream: DuplexStream = acquire_host_channel().expect("host must provide rpc stream");

    let mut rpc_system: RpcSystem<rpc_twoparty_capnp::Side> =
        guest::init_with_stream(stream);
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
    use wasip2::io::streams;

    // Use Cap'n Proto RPC primitives re-exported at the crate level.

    /// Full-duplex transport backed by WASI Preview 2 streams.
    pub struct DuplexStream {
        input: streams::InputStream,
        output: streams::OutputStream,
    }

    impl DuplexStream {
        /// Create a new duplex transport from WASI input/output resources.
        pub fn new(input: streams::InputStream, output: streams::OutputStream) -> Self {
            Self { input, output }
        }

        /// Consume the duplex transport and split it into its WASI parts.
        pub fn into_parts(self) -> (streams::InputStream, streams::OutputStream) {
            (self.input, self.output)
        }
    }

    impl From<(streams::InputStream, streams::OutputStream)> for DuplexStream {
        fn from((input, output): (streams::InputStream, streams::OutputStream)) -> Self {
            Self::new(input, output)
        }
    }

    /// Initialize a Cap'n Proto RPC system using a full-duplex transport.
    pub fn init_with_stream(stream: DuplexStream) -> RpcSystem<rpc_twoparty_capnp::Side> {
        let (input, output) = stream.into_parts();
        init_with_streams(rpc_twoparty_capnp::Side::Client, input, output)
    }

    fn init_with_streams(
        side: rpc_twoparty_capnp::Side,
        input: streams::InputStream,
        output: streams::OutputStream,
    ) -> RpcSystem<rpc_twoparty_capnp::Side> {
        init_with_streams_and_side(side, input, output)
    }

    fn init_with_streams_and_side(
        side: rpc_twoparty_capnp::Side,
        input: streams::InputStream,
        output: streams::OutputStream,
    ) -> RpcSystem<rpc_twoparty_capnp::Side> {
        let stdin = Wasip2Stdin::new(input);
        let stdout = Wasip2Stdout::new(output);
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
