#![feature(wasip2)]

use wasip2::cli::stderr::get_stderr;

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

    wetware_guest::run(|host: peer_capnp::host::Client| async move {
        log::trace!("child-echo: rpc bootstrapped");

        let executor = host.executor_request().send().pipeline.get_executor();

        // Echo call 1
        let mut req1 = executor.echo_request();
        req1.get().set_message("Hello from call 1");
        log::trace!("child-echo: sending echo request 1");
        let resp1 = req1.send().promise.await?;
        let text1 = resp1.get()?.get_response()?.to_str()?;
        log::trace!("child-echo: call 1 response: {}", text1);

        // Echo call 2
        let mut req2 = executor.echo_request();
        req2.get().set_message("Hello from call 2");
        log::trace!("child-echo: sending echo request 2");
        let resp2 = req2.send().promise.await?;
        let text2 = resp2.get()?.get_response()?.to_str()?;
        log::trace!("child-echo: call 2 response: {}", text2);

        log::trace!("child-echo: both calls complete, exiting");
        Ok(())
    });

    log::trace!("child-echo: cleanup complete");
}
