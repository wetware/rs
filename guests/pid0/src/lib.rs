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

    // Bootstrap a Membrane(WetwareSession) instead of a bare Host.
    // The membrane provides epoch-scoped sessions with Host + Executor.
    wetware_guest::run(
        |membrane: stem_capnp::membrane::Client<membrane_capnp::wetware_session::Owned>| async move {
            log::trace!("pid0: rpc bootstrapped, grafting onto membrane");

            // Graft onto the membrane to get an epoch-scoped session.
            // No signer needed — stem's graft() currently ignores it.
            let graft_resp = membrane.graft_request().send().promise.await?;
            let session = graft_resp.get()?.get_session()?;
            let ext = session.get_extension()?;
            log::trace!("pid0: grafted, got session");

            let executor = ext.get_executor()?;

            const CHILD_WASM: &[u8] =
                include_bytes!("../../child-echo/target/wasm32-wasip2/release/child_echo.wasm");

            let mut request = executor.run_bytes_request();
            {
                let mut params = request.get();
                params.set_wasm(CHILD_WASM);
                params.reborrow().init_args(0);
                params.reborrow().init_env(0);
            }
            log::trace!("pid0: runBytes sent");

            let run_resp = request.send().promise.await?;
            let process = run_resp.get()?.get_process()?;
            log::trace!("pid0: got process");

            let wait_resp = process.wait_request().send().promise.await?;
            let exit_code = wait_resp.get()?.get_exit_code();
            log::trace!("pid0: child exited with code {}", exit_code);

            Ok(())
        },
    );

    log::trace!("pid0: cleanup complete");
}

wasip2::cli::command::export!(Pid0);
