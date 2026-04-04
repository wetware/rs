use std::env;
use std::path::Path;

/// Build script for the MCP cell.
///
/// Compiles shared system schemas needed for membrane grafting.
fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_path = Path::new(&manifest_dir);

    let capnp_dir = manifest_path
        .join("../..")
        .join("capnp")
        .canonicalize()
        .expect("capnp dir not found");

    // Shared schemas needed for membrane graft.
    // crate_provides tells capnpc that schema.capnp types live in the `capnp`
    // crate, so generated code emits `::capnp::schema_capnp::node` instead of
    // `crate::schema_capnp::node`.
    capnpc::CompilerCommand::new()
        .src_prefix(&capnp_dir)
        .crate_provides("capnp", [0xa93fc509624c72d9])
        .file(capnp_dir.join("system.capnp"))
        .file(capnp_dir.join("routing.capnp"))
        .file(capnp_dir.join("stem.capnp"))
        .file(capnp_dir.join("http.capnp"))
        .run()
        .expect("failed to compile shared capnp schemas");

    // Cargo rebuild triggers
    for schema in &["system", "routing", "stem", "http"] {
        println!(
            "cargo:rerun-if-changed={}",
            capnp_dir.join(format!("{schema}.capnp")).display()
        );
    }
}
