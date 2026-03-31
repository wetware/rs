//! wagi-guest — thin helper for writing WAGI cells.
//!
//! WAGI (WebAssembly Gateway Interface) is CGI for WASM. The host injects
//! HTTP request metadata as environment variables and pipes the request
//! body to stdin. The guest reads env vars, reads stdin, and writes a
//! CGI-formatted response to stdout.
//!
//! This crate wraps the boring parts so your handler is ~8 lines:
//!
//! ```ignore
//! use wagi_guest as wagi;
//!
//! fn handle() {
//!     match wagi::method().as_str() {
//!         "GET"  => wagi::respond(200, &[("Content-Type", "text/plain")], "hello"),
//!         "POST" => wagi::respond(200, &[("Content-Type", "text/plain")], "created"),
//!         _      => wagi::respond(405, &[], "Method Not Allowed"),
//!     }
//! }
//! ```

use std::io::Read;

/// The HTTP method (GET, POST, etc.) from `REQUEST_METHOD`.
pub fn method() -> String {
    std::env::var("REQUEST_METHOD").unwrap_or_default()
}

/// The request path from `PATH_INFO`.
pub fn path() -> String {
    std::env::var("PATH_INFO").unwrap_or_default()
}

/// The query string from `QUERY_STRING` (without the leading `?`).
pub fn query() -> String {
    std::env::var("QUERY_STRING").unwrap_or_default()
}

/// Read an HTTP header by name. Header names are case-insensitive;
/// CGI stores them as `HTTP_UPPER_SNAKE_CASE`.
///
/// ```ignore
/// wagi::header("Accept") // reads HTTP_ACCEPT
/// wagi::header("X-Custom") // reads HTTP_X_CUSTOM
/// ```
pub fn header(name: &str) -> Option<String> {
    let key = format!("HTTP_{}", name.to_uppercase().replace('-', "_"));
    std::env::var(key).ok()
}

/// The `Content-Type` header (from `CONTENT_TYPE`, not `HTTP_CONTENT_TYPE`).
pub fn content_type() -> Option<String> {
    std::env::var("CONTENT_TYPE").ok()
}

/// The `Content-Length` header (from `CONTENT_LENGTH`, not `HTTP_CONTENT_LENGTH`).
pub fn content_length() -> Option<usize> {
    std::env::var("CONTENT_LENGTH")
        .ok()
        .and_then(|s| s.parse().ok())
}

/// Read the request body from stdin.
pub fn body() -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);
    buf
}

/// Read the request body from stdin as a UTF-8 string.
/// Returns an empty string if the body is not valid UTF-8.
pub fn body_string() -> String {
    String::from_utf8(body()).unwrap_or_default()
}

/// Write a CGI response to stdout.
///
/// Formats the Status line, headers, and body per RFC 3875.
/// ```ignore
/// wagi::respond(200, &[("Content-Type", "text/plain")], "hello");
/// ```
pub fn respond(status: u16, headers: &[(&str, &str)], body: &str) {
    print!("Status: {status} {}\r\n", reason_phrase(status));
    for (k, v) in headers {
        print!("{k}: {v}\r\n");
    }
    print!("\r\n{body}");
}

/// Write a CGI response with a byte body.
pub fn respond_bytes(status: u16, headers: &[(&str, &str)], body: &[u8]) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "Status: {status} {}\r\n", reason_phrase(status));
    for (k, v) in headers {
        let _ = write!(out, "{k}: {v}\r\n");
    }
    let _ = write!(out, "\r\n");
    let _ = out.write_all(body);
    let _ = out.flush();
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}
