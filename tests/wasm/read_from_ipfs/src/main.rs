use std::fs;

fn main() {
    loop {
        let file_content =
            fs::read_to_string("/ipfs/QmeeD4LBwMxMkboCNvsoJ2aDwJhtsjFyoS1B9iMXiDcEqH");
        match file_content {
            Ok(s) => println!("{}", s),
            Err(e) => println!("Error reading from file: {}", e),
        }
    }
}
