#![feature(wasip2)]

use std::io::{self, Write};

/// The default kernel now simply greets the operator and exits.
#[no_mangle]
pub extern "C" fn _start() {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "Hello, Wetware!").expect("write to stdout");
}
