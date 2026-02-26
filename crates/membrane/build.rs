use std::path::PathBuf;

fn main() {
    let capnp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../capnp");

    capnpc::CompilerCommand::new()
        .src_prefix("../../")
        .file(capnp_dir.join("system.capnp"))
        .file(capnp_dir.join("ipfs.capnp"))
        .file(capnp_dir.join("routing.capnp"))
        .file(capnp_dir.join("stem.capnp"))
        .run()
        .expect("capnp compile schemas");

    println!(
        "cargo:rerun-if-changed={}",
        capnp_dir.join("system.capnp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        capnp_dir.join("ipfs.capnp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        capnp_dir.join("routing.capnp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        capnp_dir.join("stem.capnp").display()
    );
}
