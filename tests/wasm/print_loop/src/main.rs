use std::{thread, time::Duration};

fn main() {
    loop {
        println!("nop");
        thread::sleep(Duration::from_secs(1));
    }
}
