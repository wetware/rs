#![feature(wasip2)]

use wasip2::cli::stderr::get_stderr;
use wetware_guest::{block_on, RpcSession};

#[allow(dead_code)]
mod peer_capnp {
    include!(concat!(
        env!("OUT_DIR"),
        "/Users/mikel/Code/github.com/wetware/rs/capnp/peer_capnp.rs"
    ));
}

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Trace
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let stderr = get_stderr();
        let _ = stderr.blocking_write_and_flush(
            format!("[{}] {}\n", record.level(), record.args()).as_bytes(),
        );
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn init_logging() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

#[no_mangle]
pub extern "C" fn _start() {
    init_logging();
    log::trace!("child-echo: start");

    let mut session = RpcSession::<peer_capnp::host::Client>::connect();
    let host = session.client.clone();
    log::trace!("child-echo: rpc bootstrapped");

    // Get executor via pipelining
    let executor = host.executor_request().send().pipeline.get_executor();

    // Echo call 1
    let mut req1 = executor.echo_request();
    req1.get().set_message("Hello from call 1");
    log::trace!("child-echo: sending echo request 1");
    let resp1 = block_on(&mut session, req1.send().promise)
        .expect("RPC system terminated")
        .expect("echo call 1 failed");
    let text1 = resp1
        .get()
        .expect("resp1.get() failed")
        .get_response()
        .expect("get_response failed")
        .to_str()
        .expect("to_str failed");
    log::trace!("child-echo: call 1 response: {}", text1);

    // Echo call 2
    let mut req2 = executor.echo_request();
    req2.get().set_message("Hello from call 2");
    log::trace!("child-echo: sending echo request 2");
    let resp2 = block_on(&mut session, req2.send().promise)
        .expect("RPC system terminated")
        .expect("echo call 2 failed");
    let text2 = resp2
        .get()
        .expect("resp2.get() failed")
        .get_response()
        .expect("get_response failed")
        .to_str()
        .expect("to_str failed");
    log::trace!("child-echo: call 2 response: {}", text2);

    log::trace!("child-echo: both calls complete, exiting");

    std::mem::forget(resp1);
    std::mem::forget(resp2);
    std::mem::forget(executor);
    std::mem::forget(host);
    session.forget();
    log::trace!("child-echo: cleanup complete");
}
