#![feature(wasip2)]

use futures::future::FutureExt;
use wasip2::cli::stderr::get_stderr;
use wetware_guest::{DriveOutcome, RpcDriver, RpcSession};

use std::task::Poll;

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

    let mut session = RpcSession::<peer_capnp::executor::Client>::connect();
    let executor = session.client.clone();
    log::trace!("child-echo: rpc bootstrapped");

    // Make TWO concurrent echo calls
    let mut request1 = executor.echo_request();
    request1.get().set_message("Hello from call 1");
    let mut promise1 = request1.send().promise;

    let mut request2 = executor.echo_request();
    request2.get().set_message("Hello from call 2");
    let mut promise2 = request2.send().promise;

    log::trace!("child-echo: sent both echo requests");

    let driver = RpcDriver::new();
    let mut response1_done = false;
    let mut response2_done = false;

    driver.spin_until(&mut session.rpc_system, |cx| {
        let mut progressed = false;

        if !response1_done {
            log::trace!("child-echo: polling promise1");
            match promise1.poll_unpin(cx) {
                Poll::Ready(Ok(resp)) => {
                    log::trace!("child-echo: promise1 Ready(Ok)");
                    match resp.get() {
                        Ok(reader) => match reader.get_response() {
                            Ok(text) => match text.to_str() {
                                Ok(s) => {
                                    log::trace!("child-echo: call 1 response: {}", s);
                                    response1_done = true;
                                    progressed = true;
                                }
                                Err(e) => {
                                    log::error!("child-echo: call 1 to_str failed: {:?}", e);
                                    return DriveOutcome::done();
                                }
                            },
                            Err(e) => {
                                log::error!("child-echo: call 1 get_response failed: {:?}", e);
                                return DriveOutcome::done();
                            }
                        },
                        Err(e) => {
                            log::error!("child-echo: call 1 resp.get() failed: {:?}", e);
                            return DriveOutcome::done();
                        }
                    }
                }
                Poll::Ready(Err(err)) => {
                    log::error!("child-echo: call 1 failed: {}", err);
                    return DriveOutcome::done();
                }
                Poll::Pending => {
                    log::trace!("child-echo: promise1 Pending");
                }
            }
        }

        if !response2_done {
            log::trace!("child-echo: polling promise2");
            match promise2.poll_unpin(cx) {
                Poll::Ready(Ok(resp)) => {
                    log::trace!("child-echo: promise2 Ready(Ok)");
                    match resp.get() {
                        Ok(reader) => match reader.get_response() {
                            Ok(text) => match text.to_str() {
                                Ok(s) => {
                                    log::trace!("child-echo: call 2 response: {}", s);
                                    response2_done = true;
                                    progressed = true;
                                }
                                Err(e) => {
                                    log::error!("child-echo: call 2 to_str failed: {:?}", e);
                                    return DriveOutcome::done();
                                }
                            },
                            Err(e) => {
                                log::error!("child-echo: call 2 get_response failed: {:?}", e);
                                return DriveOutcome::done();
                            }
                        },
                        Err(e) => {
                            log::error!("child-echo: call 2 resp.get() failed: {:?}", e);
                            return DriveOutcome::done();
                        }
                    }
                }
                Poll::Ready(Err(err)) => {
                    log::error!("child-echo: call 2 failed: {}", err);
                    return DriveOutcome::done();
                }
                Poll::Pending => {
                    log::trace!("child-echo: promise2 Pending");
                }
            }
        }

        if response1_done && response2_done {
            log::trace!("child-echo: both calls complete, exiting");
            return DriveOutcome::done();
        }

        if progressed {
            DriveOutcome::progress()
        } else {
            DriveOutcome::pending()
        }
    });

    // Cleanup - forget resources to avoid "resource has children" errors
    std::mem::forget(promise1);
    std::mem::forget(promise2);
    session.forget();

    log::trace!("child-echo: cleanup complete");
}
