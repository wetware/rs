#![feature(wasip2)]

use wasip2::cli::stderr::get_stderr;
use wasip2::cli::stdin::get_stdin;
use wasip2::cli::stdout::get_stdout;
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

    wetware_guest::run(
        |membrane: Membrane| async move {
            let graft_resp = membrane.graft_request().send().promise.await?;
            let session = graft_resp.get()?.get_session()?;
            let ext = session.get_extension()?;

            let executor = ext.get_executor()?;
            let stdin = get_stdin();
            let stdout = get_stdout();
            let mut buf: Vec<u8> = Vec::new();

            'outer: loop {
                // Fill buffer until we have a newline or EOF.
                loop {
                    if buf.contains(&b'\n') {
                        break;
                    }
                    match stdin.blocking_read(4096) {
                        Ok(b) if b.is_empty() => break 'outer,
                        Ok(b) => buf.extend_from_slice(&b),
                        Err(_) => break 'outer,
                    }
                }

                // Drain complete lines from the buffer.
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line_bytes = buf.drain(..=pos).collect::<Vec<_>>();
                    let line = match std::str::from_utf8(&line_bytes) {
                        Ok(s) => s.trim_end_matches('\n').trim_end_matches('\r'),
                        Err(_) => continue,
                    };

                    if line.is_empty() {
                        continue;
                    }

                    let mut req = executor.echo_request();
                    req.get().set_message(line);
                    let resp = req.send().promise.await?;
                    let text = resp.get()?.get_response()?.to_str()?;
                    let _ = stdout.blocking_write_and_flush(format!("{text}\n").as_bytes());
                }
            }

            // Flush any partial line left in the buffer.
            if !buf.is_empty() {
                let line = match std::str::from_utf8(&buf) {
                    Ok(s) => s.trim_end_matches('\r'),
                    Err(_) => "",
                };
                if !line.is_empty() {
                    let mut req = executor.echo_request();
                    req.get().set_message(line);
                    let resp = req.send().promise.await?;
                    let text = resp.get()?.get_response()?.to_str()?;
                    let _ = stdout.blocking_write_and_flush(format!("{text}\n").as_bytes());
                }
            }

            Ok(())
        },
    );

}

wasip2::cli::command::export!(Pid0);
