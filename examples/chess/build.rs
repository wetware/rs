use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_path = Path::new(&manifest_dir);
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
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
    // Save the raw CodeGeneratorRequest so we can extract canonical schema bytes.
    let raw_request = out_dir.join("chess_request.bin");
    capnpc::CompilerCommand::new()
        .src_prefix(manifest_path)
        .file(&local_schema)
        .raw_code_generator_request_path(&raw_request)
        .run()
        .expect("failed to compile chess.capnp");

    // Extract canonical schema bytes and CIDs for the ChessEngine interface.
    let schemas = schema_id::extract_schemas(
        &raw_request,
        &[("CHESS_ENGINE", 0xd0ac8299df079c61)],
    )
    .expect("extract ChessEngine schema");

    schema_id::emit_schema_consts(&out_dir.join("schema_ids.rs"), &schemas)
        .expect("emit schema consts");

    // Write raw schema bytes for post-build injection into WASM custom section.
    schema_id::write_schema_bytes(&out_dir.join("chess_engine_schema.bin"), &schemas[0])
        .expect("write schema bytes");

    for schema in &["system", "ipfs", "routing", "stem"] {
        println!(
            "cargo:rerun-if-changed={}",
            capnp_dir.join(format!("{schema}.capnp")).display()
        );
    }
    println!("cargo:rerun-if-changed={}", local_schema.display());
}
