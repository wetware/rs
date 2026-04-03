use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_path = Path::new(&manifest_dir);
    let target_dir = manifest_path.join("target");

    // Compile example schemas so integration tests get typed access.
    let greeter_schema = manifest_path.join("examples/discovery/greeter.capnp");
    if greeter_schema.exists() {
        capnpc::CompilerCommand::new()
            .src_prefix(manifest_path.join("examples/discovery"))
            .file(&greeter_schema)
            .run()
            .expect("failed to compile greeter.capnp");
        println!("cargo:rerun-if-changed={}", greeter_schema.display());
    }

    // Compile shell schema so the ww shell CLI gets typed access.
    let shell_schema = manifest_path.join("capnp/shell.capnp");
    if shell_schema.exists() {
        capnpc::CompilerCommand::new()
            .src_prefix(manifest_path.join("capnp"))
            .file(&shell_schema)
            .run()
            .expect("failed to compile shell.capnp");
        println!("cargo:rerun-if-changed={}", shell_schema.display());
    }
    let cid_file = target_dir.join("default-config.cid");

    // Read CID from the generated .cid file in target directory
    let cid_value = if cid_file.exists() {
        match fs::read_to_string(&cid_file) {
            Ok(content) => {
                let cid = content.trim();
                if cid.is_empty() {
                    String::new()
                } else {
                    format!("/ipfs/{cid}")
                }
            }
            Err(_) => {
                // Failed to read file - use empty CID
                String::new()
            }
        }
    } else {
        // File doesn't exist - this is expected on first build or when IPFS is unavailable
        // Use empty string as default (will be empty CID at runtime)
        // The Makefile will generate this file as part of 'make all' or 'make default-config'
        // Ensure target directory exists for when Makefile creates the file
        let _ = fs::create_dir_all(&target_dir);
        String::new()
    };

    // Set the environment variable for use in Rust code
    println!("cargo:rustc-env=DEFAULT_KERNEL_CID={cid_value}");
    println!("cargo:rerun-if-changed={}", cid_file.display());
}
