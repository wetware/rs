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
const PROMPT: &[u8] = b"ww> ";

#[inline]
fn write_prompt(stdout: &wasip2::io::streams::OutputStream) {
    let _ = stdout.blocking_write_and_flush(PROMPT);
}

fn init_logging() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

struct Shell;

impl Guest for Shell {
    fn run() -> Result<(), ()> {
        run_impl();
        Ok(())
    }
}

/// Shell implementation with proper command parsing and execution
fn run_impl() {
    init_logging();
    log::trace!("shell: start");

    let stdin = get_stdin();
    let stdout = get_stdout();
    let stderr = get_stderr();

    // Print prompt
    write_prompt(&stdout);

    let mut buffer = Vec::new();

    loop {
        // Wait for input to be available
        let pollable = stdin.subscribe();
        wasip2::io::poll::poll(&[&pollable]);

        // Read available data
        match stdin.read(4096) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    break;
                }

                // Add bytes to buffer
                buffer.extend_from_slice(&bytes);

                // Process complete lines
                while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                    // Extract the line (excluding newline)
                    let line_bytes = buffer.drain(..=newline_pos).collect::<Vec<_>>();
                    let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
                    let line = line.trim();

                    // Skip empty lines
                    if line.is_empty() {
                        write_prompt(&stdout);
                        continue;
                    }

                    log::trace!("shell: executing command: {}", line);

                    // Parse the command line using shlex for proper shell-style parsing
                    match shlex::split(line) {
                        Some(args) if !args.is_empty() => {
                            // Execute the command
                            if let Err(e) = execute_command(&args, &stdout) {
                                let error_msg = format!("Error: {}\n", e);
                                let _ = stderr.blocking_write_and_flush(error_msg.as_bytes());
                            }
                        }
                        Some(_) => {
                            // Empty args after parsing
                        }
                        None => {
                            let _ =
                                stdout.blocking_write_and_flush(b"Error: Invalid command syntax\n");
                        }
                    }

                    // Print prompt for next command
                    write_prompt(&stdout);
                }
            }
            Err(wasip2::io::streams::StreamError::Closed) => break,
            Err(e) => {
                log::error!("shell: stdin error: {:?}", e);
                break;
            }
        }
    }

    log::trace!("shell: exit");
}

/// Execute a parsed command
fn execute_command(
    args: &[String],
    stdout: &wasip2::io::streams::OutputStream,
) -> Result<(), String> {
    let command = &args[0];

    match command.as_str() {
        "echo" => {
            // Echo command: print all arguments separated by spaces
            if args.len() > 1 {
                let output = args[1..].join(" ");
                let output_with_newline = format!("{}\n", output);
                stdout
                    .blocking_write_and_flush(output_with_newline.as_bytes())
                    .map_err(|e| format!("Failed to write to stdout: {:?}", e))?;
            } else {
                // Just echo a newline if no arguments
                stdout
                    .blocking_write_and_flush(b"\n")
                    .map_err(|e| format!("Failed to write to stdout: {:?}", e))?;
            }
            Ok(())
        }
        "exit" => {
            // Exit command: gracefully terminate the shell
            log::trace!("shell: exit command received");
            std::process::exit(0);
        }
        "help" => {
            // Help command: show available commands
            let help_text = "Available commands:\n\
                           - echo [args...]  Echo the arguments\n\
                           - help            Show this help message\n\
                           - exit            Exit the shell\n";
            stdout
                .blocking_write_and_flush(help_text.as_bytes())
                .map_err(|e| format!("Failed to write to stdout: {:?}", e))?;
            Ok(())
        }
        _ => {
            let error_msg = format!("Unknown command: {}\n", command);
            stdout
                .blocking_write_and_flush(error_msg.as_bytes())
                .map_err(|e| format!("Failed to write to stdout: {:?}", e))?;
            Ok(())
        }
    }
}

wasip2::cli::command::export!(Shell);
