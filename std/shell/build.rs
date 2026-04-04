use std::env;
use std::path::{Path, PathBuf};

/// Build script for the shell cell.
///
/// Compiles shell.capnp and shared system schemas, extracts the
/// Shell interface's canonical bytes, and derives its schema CID.
fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_path = Path::new(&manifest_dir);
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let capnp_dir = manifest_path
        .join("../..")
        .join("capnp")
        .canonicalize()
        .expect("capnp dir not found");

    let shell_schema = capnp_dir
        .join("shell.capnp")
        .canonicalize()
        .expect("shell.capnp not found in capnp/");

    // Pass 1: shared schemas
    capnpc::CompilerCommand::new()
        .src_prefix(&capnp_dir)
        .file(capnp_dir.join("system.capnp"))
        .file(capnp_dir.join("ipfs.capnp"))
        .file(capnp_dir.join("routing.capnp"))
        .file(capnp_dir.join("stem.capnp"))
        .file(capnp_dir.join("http.capnp"))
        .run()
        .expect("failed to compile shared capnp schemas");

    // Pass 2: shell schema + schema CID
    let raw_request = out_dir.join("shell_request.bin");
    capnpc::CompilerCommand::new()
        .src_prefix(&capnp_dir)
        .file(&shell_schema)
        .raw_code_generator_request_path(&raw_request)
        .run()
        .expect("failed to compile shell.capnp");

    let shell_id = find_interface_id(&raw_request, "Shell")
        .expect("Shell interface not found in CodeGeneratorRequest");

    let schemas = schema_id::extract_schemas(&raw_request, &[("SHELL", shell_id)])
        .expect("extract Shell schema");

    schema_id::emit_schema_consts(&out_dir.join("schema_ids.rs"), &schemas)
        .expect("emit schema consts");

    schema_id::write_schema_bytes(&out_dir.join("shell_schema.bin"), &schemas[0])
        .expect("write schema bytes");

    // Pass 3: auction schema — needed for :compare handler to dial ComputeProviders.
    let auction_schema = manifest_path
        .join("../../examples/auction/auction.capnp")
        .canonicalize()
        .expect("auction.capnp not found in examples/auction/");

    let auction_raw = out_dir.join("auction_request.bin");
    capnpc::CompilerCommand::new()
        .src_prefix(
            auction_schema
                .parent()
                .expect("auction.capnp parent dir"),
        )
        .file(&auction_schema)
        .raw_code_generator_request_path(&auction_raw)
        .run()
        .expect("failed to compile auction.capnp");

    let provider_id = find_interface_id(&auction_raw, "ComputeProvider")
        .expect("ComputeProvider interface not found in CodeGeneratorRequest");

    let auction_schemas =
        schema_id::extract_schemas(&auction_raw, &[("COMPUTE_PROVIDER", provider_id)])
            .expect("extract ComputeProvider schema");

    // Append auction schema consts to the existing schema_ids.rs.
    let auction_consts_path = out_dir.join("auction_schema_ids.rs");
    schema_id::emit_schema_consts(&auction_consts_path, &auction_schemas)
        .expect("emit auction schema consts");

    schema_id::write_schema_bytes(&out_dir.join("auction_schema.bin"), &auction_schemas[0])
        .expect("write auction schema bytes");

    // Cargo rebuild triggers
    for schema in &["system", "ipfs", "routing", "stem", "http", "shell"] {
        println!(
            "cargo:rerun-if-changed={}",
            capnp_dir.join(format!("{schema}.capnp")).display()
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        auction_schema.display()
    );
}

/// Scan a raw CodeGeneratorRequest for an interface node with the given
/// display name and return its type ID.
fn find_interface_id(raw_request_path: &Path, name: &str) -> Option<u64> {
    let data = std::fs::read(raw_request_path).ok()?;
    let reader =
        capnp::serialize::read_message(&mut data.as_slice(), capnp::message::ReaderOptions::new())
            .ok()?;
    let request: capnp::schema_capnp::code_generator_request::Reader = reader.get_root().ok()?;
    for node in request.get_nodes().ok()?.iter() {
        if let Ok(n) = node.get_display_name() {
            if n.to_str().ok()?.ends_with(&format!(":{name}")) || n.to_str().ok()? == name {
                if matches!(
                    node.which(),
                    Ok(capnp::schema_capnp::node::Which::Interface(_))
                ) {
                    return Some(node.get_id());
                }
            }
        }
    }
    None
}
