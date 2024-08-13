fn main() -> Result<(), Box<dyn std::error::Error>> {
    capnpc::CompilerCommand::new()
        .file("proc.capnp")
        .run()?;
    Ok(())
}
