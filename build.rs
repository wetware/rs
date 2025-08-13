use std::env;

fn main() {
    // Tell Cargo to rerun this build script if the schema files change
    println!("cargo:rerun-if-changed=src/auth.capnp");
    
    // Generate Rust code from the Cap'n Proto schema
    let out_dir = env::var("OUT_DIR").unwrap();
    let schema_dir = "src";
    
    // For now, skip code generation due to capnpc issues
    // TODO: Fix capnpc command or use alternative approach
    println!("cargo:warning=Skipping auth.capnp compilation due to capnpc issues");
    println!("cargo:warning=OUT_DIR: {}", out_dir);
}
