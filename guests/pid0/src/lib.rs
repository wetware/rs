#![feature(wasip2)]

use wasip2::cli::stderr::get_stderr;
use wasip2::exports::cli::run::Guest;
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

struct Pid0;

impl Guest for Pid0 {
    fn run() -> Result<(), ()> {
        run_impl();
        Ok(())
    }
}

fn run_impl() {
    init_logging();
    log::trace!("pid0: start");
    const CHILD_WASM: &[u8] =
        include_bytes!("../../child-echo/target/wasm32-wasip2/release/child_echo.wasm");

    let mut session = RpcSession::<peer_capnp::host::Client>::connect();
    let host = session.client.clone();
    log::trace!("pid0: rpc bootstrapped");

    // Get executor via pipelining
    let executor = host.executor_request().send().pipeline.get_executor();

    let mut request = executor.run_bytes_request();
    {
        let mut params = request.get();
        params.set_wasm(CHILD_WASM);
        params.reborrow().init_args(0);
        params.reborrow().init_env(0);
    }
    log::trace!("pid0: runBytes sent");

    let run_resp = block_on(&mut session, request.send().promise)
        .expect("RPC system terminated")
        .expect("runBytes failed");
    let process = run_resp
        .get()
        .expect("missing executor response")
        .get_process()
        .expect("missing process");
    log::trace!("pid0: got process");

    let wait_resp = block_on(&mut session, process.wait_request().send().promise)
        .expect("RPC system terminated")
        .expect("wait failed");
    let exit_code = wait_resp.get().unwrap().get_exit_code();
    log::trace!("pid0: child exited with code {}", exit_code);

    std::mem::forget(wait_resp);
    std::mem::forget(run_resp);
    std::mem::forget(process);
    std::mem::forget(executor);
    std::mem::forget(host);
    session.forget();
    log::trace!("pid0: cleanup complete");
}

wasip2::cli::command::export!(Pid0);
