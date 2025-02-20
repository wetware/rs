use std::io;

fn echo() {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let res = io::copy(&mut stdin, &mut stdout);
    match res {
        Ok(_) => (),
        Err(e) => panic!("{}", e),
    }
}

fn main() {
    echo();
}
