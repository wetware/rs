use std::path::PathBuf;

fn main() {
    let capnp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../capnp");

    capnpc::CompilerCommand::new()
        .src_prefix("../../")
        // schema.capnp types live in the `capnp` crate, not here.
        .crate_provides("capnp", [0xa93fc509624c72d9])
        .file(capnp_dir.join("cell.capnp"))
        .run()
        .expect("capnp compile cell.capnp");

    println!(
        "cargo:rerun-if-changed={}",
        capnp_dir.join("cell.capnp").display()
    );
}
