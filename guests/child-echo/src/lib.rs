#![feature(wasip2)]

use wasip2::cli::stdout::get_stdout;

#[no_mangle]
pub extern "C" fn _start() {
    let stdout = get_stdout();
    let message = b"CHILD_OK\n";
    let _ = stdout.blocking_write_and_flush(message);
}
