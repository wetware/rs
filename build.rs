fn main() {
    println!("cargo:rerun-if-changed=src/swarm.capnp");

    capnpc::CompilerCommand::new()
        .file("src/swarm.capnp")
        .src_prefix("src/") // Strip src/ prefix from input files
        .run()
        .expect("capnp schema compilation failed");
}
