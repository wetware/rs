use capnp::capability::FromClientHook;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::task::noop_waker;
use futures::FutureExt;
use std::task::{Context, Poll, Waker};

mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "guest-streams",
    });
}

use bindings::wetware::streams::streams::{
    create_connection, InputStream as WetwareInputStream, OutputStream as WetwareOutputStream,
    Pollable as WetwarePollable, StreamError as WetwareStreamError,
};

pub struct StreamReader {
    stream: WetwareInputStream,
    buffer: Vec<u8>,
    offset: usize,
}

impl StreamReader {
    pub fn new(stream: WetwareInputStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            offset: 0,
        }
    }

    pub fn pollable(&self) -> WetwarePollable {
        self.stream.subscribe()
    }
}

impl futures::io::AsyncRead for StreamReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.offset < self.buffer.len() {
            let available = &self.buffer[self.offset..];
            let to_copy = available.len().min(buf.len());
            buf[..to_copy].copy_from_slice(&available[..to_copy]);
            self.offset += to_copy;
            if self.offset >= self.buffer.len() {
                self.buffer.clear();
                self.offset = 0;
            }
            return std::task::Poll::Ready(Ok(to_copy));
        }

        let len = buf.len() as u64;
        match self.stream.read(len) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    cx.waker().wake_by_ref();
                    return std::task::Poll::Pending;
                }
                self.buffer = bytes;
                self.offset = 0;
                let available = &self.buffer[self.offset..];
                let to_copy = available.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&available[..to_copy]);
                self.offset += to_copy;
                std::task::Poll::Ready(Ok(to_copy))
            }
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream read error: {:?}", err),
            ))),
        }
    }
}

pub struct StreamWriter {
    stream: WetwareOutputStream,
}

impl StreamWriter {
    pub fn new(stream: WetwareOutputStream) -> Self {
        Self { stream }
    }

    pub fn pollable(&self) -> WetwarePollable {
        self.stream.subscribe()
    }
}

impl futures::io::AsyncWrite for StreamWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return std::task::Poll::Ready(Ok(0));
        }
        match self.stream.check_write() {
            Ok(0) => {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
            Ok(budget) => {
                let to_write = buf.len().min(budget as usize);
                match self.stream.write(&buf[..to_write]) {
                    Ok(_written) => std::task::Poll::Ready(Ok(to_write)),
                    Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
                    Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("stream write error: {:?}", err),
                    ))),
                }
            }
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream write error: {:?}", err),
            ))),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.stream.flush() {
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream flush error: {:?}", err),
            ))),
        }
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.stream.flush() {
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream close error: {:?}", err),
            ))),
        }
    }
}

pub struct StreamPollables {
    pub reader: WetwarePollable,
    pub writer: WetwarePollable,
}

pub struct GuestStreams {
    pub reader: StreamReader,
    pub writer: StreamWriter,
    pub pollables: StreamPollables,
}

pub fn connect_streams() -> GuestStreams {
    let connection = create_connection();
    let input_stream = connection.get_input_stream();
    let output_stream = connection.get_output_stream();

    let reader = StreamReader::new(input_stream);
    let writer = StreamWriter::new(output_stream);
    let pollables = StreamPollables {
        reader: reader.pollable(),
        writer: writer.pollable(),
    };

    GuestStreams {
        reader,
        writer,
        pollables,
    }
}

pub struct RpcSession<C> {
    pub rpc_system: RpcSystem<Side>,
    pub client: C,
    pub pollables: StreamPollables,
}

impl<C: FromClientHook> RpcSession<C> {
    pub fn connect() -> Self {
        let streams = connect_streams();
        let pollables = streams.pollables;
        let network = VatNetwork::new(
            streams.reader,
            streams.writer,
            Side::Client,
            Default::default(),
        );
        let mut rpc_system = RpcSystem::new(Box::new(network), None);
        let client = rpc_system.bootstrap(Side::Server);
        Self {
            rpc_system,
            client,
            pollables,
        }
    }

    pub fn forget(self) {
        std::mem::forget(self.client);
        std::mem::forget(self.rpc_system);
        std::mem::forget(self.pollables);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DriveOutcome {
    pub done: bool,
    pub progressed: bool,
}

impl DriveOutcome {
    pub fn done() -> Self {
        Self {
            done: true,
            progressed: true,
        }
    }

    pub fn progress() -> Self {
        Self {
            done: false,
            progressed: true,
        }
    }

    pub fn pending() -> Self {
        Self {
            done: false,
            progressed: false,
        }
    }
}

pub struct RpcDriver {
    waker: Waker,
}

impl RpcDriver {
    pub fn new() -> Self {
        Self {
            waker: noop_waker(),
        }
    }

    pub fn spin_until<F>(&self, rpc_system: &mut RpcSystem<Side>, mut poll: F)
    where
        F: FnMut(&mut Context<'_>) -> DriveOutcome,
    {
        loop {
            let mut made_progress = false;
            let mut cx = Context::from_waker(&self.waker);

            match rpc_system.poll_unpin(&mut cx) {
                Poll::Ready(Ok(())) | Poll::Ready(Err(_)) => {
                    made_progress = true;
                }
                Poll::Pending => {}
            }

            let outcome = poll(&mut cx);
            made_progress |= outcome.progressed;

            if outcome.done {
                break;
            }

            if !made_progress {
                std::hint::spin_loop();
            }
        }
    }
}
