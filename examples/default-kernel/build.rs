fn main() {
    // Compile Cap'n Proto schema for guest
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let schema_path = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("src")
        .join("schema")
        .join("router.capnp");
    
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    
    if schema_path.exists() {
        capnpc::CompilerCommand::new()
            .file(&schema_path)
            .output_path(&out_dir)
            .run()
            .expect("compiling schema");
    }
}

