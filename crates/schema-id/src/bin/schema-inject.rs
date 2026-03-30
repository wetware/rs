//! Post-build tool: inject a Cell type tag into a WASM binary.
//!
//! Usage:
//!   schema-inject <wasm-file> --capnp <schema-bytes-file>
//!   schema-inject <wasm-file> --raw <protocol-id>
//!   schema-inject <wasm-file> --http <path-prefix>
//!
//! Builds a Cell capnp message, injects it as a "cell.capnp" custom section,
//! and overwrites the WASM file.

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let wasm_path = &args[1];
    let flag = &args[2];

    let cell_data = match flag.as_str() {
        "--capnp" => {
            if args.len() != 4 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            let schema_bytes = std::fs::read(&args[3]).unwrap_or_else(|e| {
                eprintln!("Failed to read schema file '{}': {}", args[3], e);
                std::process::exit(1);
            });
            let cell_msg = schema_id::build_cell_capnp_message(&schema_bytes);
            let cid = schema_id::compute_cid(&schema_bytes);
            eprintln!("Cell::capnp — schema CID: {cid}");
            cell_msg
        }
        "--raw" => {
            if args.len() != 4 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            let protocol_id = &args[3];
            eprintln!("Cell::raw — protocol: {protocol_id}");
            schema_id::build_cell_raw_message(protocol_id)
        }
        "--http" => {
            if args.len() != 4 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            let path_prefix = &args[3];
            eprintln!("Cell::http — path: {path_prefix}");
            schema_id::build_cell_http_message(path_prefix)
        }
        _ => {
            eprintln!("Unknown flag: {flag}");
            print_usage(&args[0]);
            std::process::exit(1);
        }
    };

    let wasm_bytes = std::fs::read(wasm_path).unwrap_or_else(|e| {
        eprintln!("Failed to read WASM file '{wasm_path}': {e}");
        std::process::exit(1);
    });

    let output =
        schema_id::inject_custom_section(&wasm_bytes, schema_id::CELL_SECTION_NAME, &cell_data);

    std::fs::write(wasm_path, &output).unwrap_or_else(|e| {
        eprintln!("Failed to write WASM file '{wasm_path}': {e}");
        std::process::exit(1);
    });

    eprintln!(
        "Injected '{}' ({} bytes) into {wasm_path}",
        schema_id::CELL_SECTION_NAME,
        cell_data.len(),
    );
}

fn print_usage(program: &str) {
    eprintln!("Usage:");
    eprintln!("  {program} <wasm-file> --capnp <schema-bytes-file>");
    eprintln!("  {program} <wasm-file> --raw <protocol-id>");
    eprintln!("  {program} <wasm-file> --http <path-prefix>");
}
