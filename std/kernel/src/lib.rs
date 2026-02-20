#![feature(wasip2)]

use wasip2::cli::stderr::get_stderr;
use wasip2::exports::cli::run::Guest;

#[allow(dead_code)]
mod peer_capnp {
    include!(concat!(env!("OUT_DIR"), "/peer_capnp.rs"));
}

#[allow(dead_code)]
mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/stem_capnp.rs"));
}

#[allow(dead_code)]
mod membrane_capnp {
    include!(concat!(env!("OUT_DIR"), "/membrane_capnp.rs"));
}

/// Bootstrap capability: a Membrane whose sessions carry our WetwareSession extension.
type Membrane = stem_capnp::membrane::Client<membrane_capnp::wetware_session::Owned>;

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
    log::trace!("kernel: start");

    wetware_guest::run(
        |membrane: Membrane| async move {
            log::trace!("kernel: rpc bootstrapped, grafting onto membrane");

            let graft_resp = membrane.graft_request().send().promise.await?;
            let session = graft_resp.get()?.get_session()?;
            let ext = session.get_extension()?;
            log::trace!("kernel: grafted, got session");

            let executor = ext.get_executor()?;

            let mut req = executor.echo_request();
            req.get().set_message("hello from kernel");
            log::trace!("kernel: sending echo request");

            let resp = req.send().promise.await?;
            let text = resp.get()?.get_response()?.to_str()?;
            log::trace!("kernel: echo response: {}", text);

            Ok(())
        },
    );

    log::trace!("kernel: cleanup complete");
}

wasip2::cli::command::export!(Pid0);
