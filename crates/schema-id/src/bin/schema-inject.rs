//! Post-build tool: inject a custom section into a WASM binary.
//!
//! Usage:
//!   schema-inject <wasm-file> <section-name> <data-file>
//!
//! Reads the WASM binary, appends a custom section with the given name
//! containing the bytes from data-file, and overwrites the WASM file.
//!
//! Example:
//!   schema-inject target/chess.wasm schema.capnp schema_bytes.bin

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("Usage: {} <wasm-file> <section-name> <data-file>", args[0]);
        std::process::exit(1);
    }

    let wasm_path = &args[1];
    let section_name = &args[2];
    let data_path = &args[3];

    let wasm_bytes = std::fs::read(wasm_path).unwrap_or_else(|e| {
        eprintln!("Failed to read WASM file '{}': {}", wasm_path, e);
        std::process::exit(1);
    });

    let data = std::fs::read(data_path).unwrap_or_else(|e| {
        eprintln!("Failed to read data file '{}': {}", data_path, e);
        std::process::exit(1);
    });

    let output = schema_id::inject_custom_section(&wasm_bytes, section_name, &data);

    std::fs::write(wasm_path, &output).unwrap_or_else(|e| {
        eprintln!("Failed to write WASM file '{}': {}", wasm_path, e);
        std::process::exit(1);
    });

    eprintln!(
        "Injected section '{}' ({} bytes) into {}",
        section_name,
        data.len(),
        wasm_path
    );
}
