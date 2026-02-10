#![feature(wasip2)]

use wasip2::cli::stderr::get_stderr;
use wasip2::cli::stdin::get_stdin;
use wasip2::cli::stdout::get_stdout;
use wasip2::exports::cli::run::Guest;

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

struct Shell;

impl Guest for Shell {
    fn run() -> Result<(), ()> {
        run_impl();
        Ok(())
    }
}

fn run_impl() {
    init_logging();
    log::trace!("shell: start");

    let stdin = get_stdin();
    let stdout = get_stdout();

    loop {
        let pollable = stdin.subscribe();
        wasip2::io::poll::poll(&[&pollable]);

        match stdin.read(4096) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    break;
                }
                if stdout.blocking_write_and_flush(&bytes).is_err() {
                    break;
                }
            }
            Err(wasip2::io::streams::StreamError::Closed) => break,
            Err(_) => break,
        }
    }

    log::trace!("shell: exit");
}

wasip2::cli::command::export!(Shell);
