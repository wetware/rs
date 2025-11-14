fn main() {
    // Compile Cap'n Proto schema for guest
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let schema_path = workspace_root.join("src").join("schema").join("router.capnp");
    
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    
    if schema_path.exists() {
        // Change to workspace root so capnpc uses relative paths
        let current_dir = std::env::current_dir().expect("current_dir");
        std::env::set_current_dir(workspace_root).expect("set_current_dir");
        
        capnpc::CompilerCommand::new()
            .file("src/schema/router.capnp")
            .output_path(&out_dir)
            .run()
            .expect("compiling schema");
        
        std::env::set_current_dir(&current_dir).expect("restore_current_dir");
    }
}

