// TODO: Extract a shared build-support crate so all guests use a common
// capnp compilation helper instead of duplicating this logic.

use std::env;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let capnp_dir = Path::new(&manifest_dir)
        .join("../..")
        .join("capnp")
        .canonicalize()
        .expect("capnp dir not found");

    capnpc::CompilerCommand::new()
        .src_prefix(&capnp_dir)
        .file(capnp_dir.join("peer.capnp"))
        .run()
        .expect("failed to compile capnp schema");
    println!(
        "cargo:rerun-if-changed={}",
        capnp_dir.join("peer.capnp").display()
    );
}
