fn main() {
    println!("cargo:rerun-if-changed=src/swarm.capnp");

    capnpc::CompilerCommand::new()
        .file("src/swarm.capnp")
        .src_prefix("src/") // Strip src/ prefix from input files
        .run()
        .expect("capnp schema compilation failed");

    // Add dead_code allow annotation to generated file
    let generated_file = "src/swarm_capnp.rs";
    if std::path::Path::new(generated_file).exists() {
        let content =
            std::fs::read_to_string(generated_file).expect("Failed to read generated file");

        // Only add the annotation if it's not already present
        if !content.starts_with("#![allow(dead_code)]") {
            let modified_content = format!("#![allow(dead_code)]\n{}", content);

            std::fs::write(generated_file, modified_content)
                .expect("Failed to write modified generated file");
        }
    }
}
