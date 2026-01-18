use std::env;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let capnp_file = Path::new(&manifest_dir)
        .join("..")
        .join("..")
        .join("capnp")
        .join("peer.capnp");

    capnpc::CompilerCommand::new()
        .file(&capnp_file)
        .run()
        .expect("failed to compile capnp schema");
    println!("cargo:rerun-if-changed={}", capnp_file.display());
}
