fn main() {
    println!("cargo:rerun-if-changed=src/auth.capnp");

    capnpc::CompilerCommand::new()
        .file("src/auth.capnp")
        .src_prefix("src/") // Strip src/ prefix from input files
        .run()
        .expect("capnp schema compilation failed");
}
