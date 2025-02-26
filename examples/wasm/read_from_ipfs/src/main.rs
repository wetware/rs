use std::fs;

fn main() {
    let file_content = fs::read_to_string("/ipfs/QmeeLUVdiSTTKQqhWqsffYDtNvvvcTfJdotkNyi1KDEJtQ");
    match file_content {
        Ok(s) => assert_eq!(s, String::from("Hello, world!")),
        Err(e) => panic!("Error reading from file: {}", e),
    }
}
