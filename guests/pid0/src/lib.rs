#![feature(wasip2)]

use futures::future::FutureExt;
use wasip2::cli::stderr::get_stderr;
use wasip2::exports::cli::run::Guest;
use wetware_guest::{DriveOutcome, RpcDriver, RpcSession};

use std::task::Poll;

mod peer_capnp;

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

    let mut session = RpcSession::<peer_capnp::executor::Client>::connect();
    let executor = session.client.clone();
    log::trace!("pid0: rpc bootstrapped");

    let mut request = executor.run_bytes_request();
    {
        let mut params = request.get();
        params.set_wasm(CHILD_WASM);
        params.reborrow().init_args(0);
        params.reborrow().init_env(0);
    }
    let mut response = request.send().promise;
    log::trace!("pid0: runBytes sent");
    let driver = RpcDriver::new();
    let mut wait_promise = None;

    driver.drive_until(&mut session.rpc_system, &session.pollables, |cx| {
        let mut progressed = false;

        if wait_promise.is_none() {
            match response.poll_unpin(cx) {
                Poll::Ready(Ok(resp)) => {
                    let process = resp
                        .get()
                        .expect("missing executor response")
                        .get_process()
                        .expect("missing process");
                    wait_promise = Some(process.wait_request().send().promise);
                    log::trace!("pid0: got process");
                    progressed = true;
                }
                Poll::Ready(Err(err)) => {
                    panic!("executor call failed: {err}");
                }
                Poll::Pending => {}
            }
        } else if let Some(wait) = wait_promise.as_mut() {
            match wait.poll_unpin(cx) {
                Poll::Ready(Ok(_)) => {
                    log::trace!("pid0: child exited");
                    return DriveOutcome::done();
                }
                Poll::Ready(Err(err)) => {
                    panic!("wait failed: {err}");
                }
                Poll::Pending => {}
            }
        }

        if progressed {
            DriveOutcome::progress()
        } else {
            DriveOutcome::pending()
        }
    });

    if let Some(promise) = wait_promise {
        std::mem::forget(promise);
    }
    std::mem::forget(response);
    std::mem::forget(executor);
    session.forget();
    log::trace!("pid0: cleanup complete");
}

wasip2::cli::command::export!(Pid0);
