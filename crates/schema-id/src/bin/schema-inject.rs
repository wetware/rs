//! Post-build tool: inject a Cell type tag into a WASM binary.
//!
//! Usage:
//!   schema-inject [--no-ipfs] <wasm-file> --capnp <schema-bytes-file>
//!   schema-inject <wasm-file> --raw <protocol-id>
//!   schema-inject <wasm-file> --http <path-prefix>
//!
//! Builds a Cell capnp message, injects it as a "cell.capnp" custom section,
//! and overwrites the WASM file.
//!
//! In --capnp mode, if Kubo (IPFS) is detected on PATH, the schema bytes
//! are pushed to IPFS via `ipfs block put --mhtype=blake3 --cid-codec=raw`.
//! The returned CID is compared against the locally computed CID.
//! Pass --no-ipfs to skip the IPFS push.

use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let no_ipfs = args.iter().any(|a| a == "--no-ipfs");
    let positional: Vec<&String> = args[1..].iter().filter(|a| *a != "--no-ipfs").collect();

    if positional.len() < 2 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let wasm_path = positional[0];
    let flag = positional[1];

    let cell_data = match flag.as_str() {
        "--capnp" => {
            if positional.len() != 3 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            let schema_bytes = std::fs::read(positional[2]).unwrap_or_else(|e| {
                eprintln!("Failed to read schema file '{}': {}", positional[2], e);
                std::process::exit(1);
            });
            if schema_bytes.is_empty() {
                eprintln!("Error: schema file '{}' is empty", positional[2]);
                std::process::exit(1);
            }
            let cell_msg = schema_id::build_cell_capnp_message(&schema_bytes);
            let cid = schema_id::compute_cid(&schema_bytes);
            eprintln!("Cell::capnp — schema CID: {cid}");

            if !no_ipfs {
                try_ipfs_push(&schema_bytes, &cid);
            }

            cell_msg
        }
        "--raw" => {
            if positional.len() != 3 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            let protocol_id = positional[2];
            eprintln!("Cell::raw — protocol: {protocol_id}");
            schema_id::build_cell_raw_message(protocol_id)
        }
        "--http" => {
            if positional.len() != 3 {
                print_usage(&args[0]);
                std::process::exit(1);
            }
            let path_prefix = positional[2];
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
    eprintln!("  {program} [--no-ipfs] <wasm-file> --capnp <schema-bytes-file>");
    eprintln!("  {program} <wasm-file> --raw <protocol-id>");
    eprintln!("  {program} <wasm-file> --http <path-prefix>");
}

/// Best-effort push of schema bytes to IPFS via Kubo CLI.
///
/// Never fails the build. All errors are printed as diagnostics.
fn try_ipfs_push(data: &[u8], expected_cid: &str) {
    // Fast PATH check: is `ipfs` even installed?
    let which = Command::new("which").arg("ipfs").output();
    if which.is_err() || !which.unwrap().status.success() {
        eprintln!("Kubo not detected on PATH. Schema available locally only.");
        return;
    }

    // Version check (no daemon needed)
    if let Ok(out) = Command::new("ipfs").arg("version").output() {
        let version = String::from_utf8_lossy(&out.stdout);
        eprintln!("IPFS: {}", version.trim());
    }

    // Push: ipfs block put --mhtype=blake3 --cid-codec=raw
    let child = Command::new("ipfs")
        .args(["block", "put", "--mhtype=blake3", "--cid-codec=raw"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to run `ipfs block put`: {e}");
            return;
        }
    };

    // Write schema bytes to stdin
    use std::io::Write;
    if let Some(ref mut stdin) = child.stdin {
        let _ = stdin.write_all(data);
    }
    drop(child.stdin.take()); // close stdin to signal EOF

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to read `ipfs block put` output: {e}");
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("ipfs block put failed: {}", stderr.trim());
        return;
    }

    let ipfs_cid = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if ipfs_cid == expected_cid {
        eprintln!("IPFS: pushed schema block, CID matches: {expected_cid}");

        // Pin to prevent GC
        let pin = Command::new("ipfs")
            .args(["pin", "add", expected_cid])
            .output();
        match pin {
            Ok(p) if p.status.success() => eprintln!("IPFS: pinned {expected_cid}"),
            Ok(p) => {
                eprintln!(
                    "IPFS: pin failed: {}",
                    String::from_utf8_lossy(&p.stderr).trim()
                )
            }
            Err(e) => eprintln!("IPFS: pin failed: {e}"),
        }
    } else {
        eprintln!(
            "WARNING: CID mismatch! Local: {expected_cid}, IPFS: {ipfs_cid}\n\
             This indicates a BLAKE3 implementation mismatch."
        );
    }
}
