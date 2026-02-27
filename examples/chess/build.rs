use std::env;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_path = Path::new(&manifest_dir);
    let capnp_dir = manifest_path
        .join("../..")
        .join("capnp")
        .canonicalize()
        .expect("capnp dir not found");
    let local_schema = manifest_path
        .join("chess.capnp")
        .canonicalize()
        .expect("chess.capnp not found next to Cargo.toml");

    // Pass 1: shared schemas from <repo>/capnp/.
    capnpc::CompilerCommand::new()
        .src_prefix(&capnp_dir)
        .file(capnp_dir.join("system.capnp"))
        .file(capnp_dir.join("ipfs.capnp"))
        .file(capnp_dir.join("routing.capnp"))
        .file(capnp_dir.join("stem.capnp"))
        .run()
        .expect("failed to compile shared capnp schemas");

    // Pass 2: chess-specific schema (local to this example).
    capnpc::CompilerCommand::new()
        .src_prefix(manifest_path)
        .file(&local_schema)
        .run()
        .expect("failed to compile chess.capnp");

    for schema in &["system", "ipfs", "routing", "stem"] {
        println!(
            "cargo:rerun-if-changed={}",
            capnp_dir.join(format!("{schema}.capnp")).display()
        );
    }
    println!("cargo:rerun-if-changed={}", local_schema.display());
}
