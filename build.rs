use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Compile Cap'n Proto schemas
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let schema_dir = Path::new(&manifest_dir).join("src").join("schema");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");

    if schema_dir.exists() {
        capnpc::CompilerCommand::new()
            .file(schema_dir.join("router.capnp"))
            .output_path(&out_dir)
            .run()
            .expect("compiling schema");
    }

    let target_dir = Path::new(&manifest_dir).join("target");
    let cid_file = target_dir.join("default-config.cid");

    // Read CID from the generated .cid file in target directory
    let cid_value = if cid_file.exists() {
        match fs::read_to_string(&cid_file) {
            Ok(content) => {
                let cid = content.trim();
                if cid.is_empty() {
                    String::new()
                } else {
                    format!("/ipfs/{}", cid)
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
    println!("cargo:rustc-env=DEFAULT_KERNEL_CID={}", cid_value);
    println!("cargo:rerun-if-changed={}", cid_file.display());
}
