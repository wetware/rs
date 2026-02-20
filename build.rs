use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let target_dir = Path::new(&manifest_dir).join("target");
    let cid_file = target_dir.join("default-config.cid");
    let capnp_dir = Path::new(&manifest_dir).join("capnp");
    let capnp_file = capnp_dir.join("peer.capnp");
    let membrane_file = capnp_dir.join("membrane.capnp");

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

    capnpc::CompilerCommand::new()
        .file(&capnp_file)
        .file(&membrane_file)
        .crate_provides("membrane", [0x9bce094a026970c4_u64]) // stem.capnp types live in the membrane crate
        .run()
        .expect("failed to compile capnp schemas");
    println!("cargo:rerun-if-changed={}", capnp_file.display());
    println!("cargo:rerun-if-changed={}", membrane_file.display());
}
