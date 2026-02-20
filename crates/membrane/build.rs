use std::path::PathBuf;

fn main() {
    let schema = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../capnp/stem.capnp");
    capnpc::CompilerCommand::new()
        .src_prefix("../../")
        .file(schema)
        .run()
        .expect("capnp compile stem.capnp");
}
