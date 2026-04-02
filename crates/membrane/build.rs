use std::path::PathBuf;

fn main() {
    let capnp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../capnp");

    capnpc::CompilerCommand::new()
        .src_prefix("../../")
        // Tell capnpc that schema.capnp types live in the `capnp` crate,
        // not in this crate. Without this, generated code for cell.capnp
        // emits `crate::schema_capnp::node` instead of `::capnp::schema_capnp::node`.
        .crate_provides("capnp", [0xa93fc509624c72d9])
        .file(capnp_dir.join("system.capnp"))
        .file(capnp_dir.join("ipfs.capnp"))
        .file(capnp_dir.join("routing.capnp"))
        .file(capnp_dir.join("stem.capnp"))
        .file(capnp_dir.join("cell.capnp"))
        .file(capnp_dir.join("http.capnp"))
        .run()
        .expect("capnp compile schemas");

    for schema in &["system", "ipfs", "routing", "stem", "cell", "http"] {
        println!(
            "cargo:rerun-if-changed={}",
            capnp_dir.join(format!("{schema}.capnp")).display()
        );
    }
}
