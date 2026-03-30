//! Counter — HTTP/FastCGI cell demo.
//!
//! Speaks the FastCGI binary protocol (spec version 1) over stdin/stdout.
//! The host sends FCGI_BEGIN_REQUEST + FCGI_PARAMS + FCGI_STDIN records.
//! The guest responds with FCGI_STDOUT + FCGI_END_REQUEST records.
//!
//! Supports:
//!   GET  /counter → "0" (or current count in Mode B)
//!   POST /counter → "1" (increments, returns new count)
//!   *             → 405 Method Not Allowed

use wasip2::cli::stdin::get_stdin;
use wasip2::cli::stdout::get_stdout;
use wasip2::exports::cli::run::Guest;
use wasip2::io::streams::{InputStream, OutputStream};

// ---------------------------------------------------------------------------
// FastCGI constants (spec version 1)
// ---------------------------------------------------------------------------

const FCGI_VERSION_1: u8 = 1;
const FCGI_HEADER_LEN: usize = 8;

// Record types
const FCGI_BEGIN_REQUEST: u8 = 1;
const FCGI_END_REQUEST: u8 = 3;
const FCGI_PARAMS: u8 = 4;
const FCGI_STDIN: u8 = 5;
const FCGI_STDOUT: u8 = 6;

// Roles
const FCGI_RESPONDER: u16 = 1;

// Protocol status
const FCGI_REQUEST_COMPLETE: u8 = 0;
const FCGI_UNKNOWN_ROLE: u8 = 3;

// ---------------------------------------------------------------------------
// FastCGI record header
// ---------------------------------------------------------------------------

struct FcgiHeader {
    version: u8,
    record_type: u8,
    request_id: u16,
    content_length: u16,
    padding_length: u8,
}

impl FcgiHeader {
    fn parse(buf: &[u8; FCGI_HEADER_LEN]) -> Self {
        Self {
            version: buf[0],
            record_type: buf[1],
            request_id: u16::from_be_bytes([buf[2], buf[3]]),
            content_length: u16::from_be_bytes([buf[4], buf[5]]),
            padding_length: buf[6],
        }
    }

    fn encode(&self) -> [u8; FCGI_HEADER_LEN] {
        let id = self.request_id.to_be_bytes();
        let len = self.content_length.to_be_bytes();
        [
            self.version,
            self.record_type,
            id[0],
            id[1],
            len[0],
            len[1],
            self.padding_length,
            0, // reserved
        ]
    }
}

// ---------------------------------------------------------------------------
// FastCGI name-value pair decoding (FCGI_PARAMS)
// ---------------------------------------------------------------------------

/// Decode one length field from FastCGI name-value encoding.
/// High bit 0 → 1-byte length. High bit 1 → 4-byte length (mask off high bit).
fn decode_nv_length(data: &[u8]) -> Option<(u32, usize)> {
    let first = *data.first()?;
    if first & 0x80 == 0 {
        Some((first as u32, 1))
    } else {
        if data.len() < 4 {
            return None;
        }
        let len = u32::from_be_bytes([first & 0x7f, data[1], data[2], data[3]]);
        Some((len, 4))
    }
}

/// Decode all name-value pairs from an FCGI_PARAMS payload.
fn decode_params(mut data: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut pairs = Vec::new();
    while !data.is_empty() {
        let (name_len, consumed1) = match decode_nv_length(data) {
            Some(v) => v,
            None => break,
        };
        data = &data[consumed1..];

        let (value_len, consumed2) = match decode_nv_length(data) {
            Some(v) => v,
            None => break,
        };
        data = &data[consumed2..];

        let name_len = name_len as usize;
        let value_len = value_len as usize;
        if data.len() < name_len + value_len {
            break;
        }

        let name = data[..name_len].to_vec();
        let value = data[name_len..name_len + value_len].to_vec();
        data = &data[name_len + value_len..];
        pairs.push((name, value));
    }
    pairs
}

// ---------------------------------------------------------------------------
// WASI stdio helpers
// ---------------------------------------------------------------------------

/// Read exactly `n` bytes from stdin. Returns None on premature EOF.
fn read_exact(stdin: &InputStream, n: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(n);
    while buf.len() < n {
        let pollable = stdin.subscribe();
        pollable.block();
        match stdin.read((n - buf.len()) as u64) {
            Ok(bytes) if bytes.is_empty() => return None, // EOF
            Ok(bytes) => buf.extend_from_slice(&bytes),
            Err(_) => return None,
        }
    }
    Some(buf)
}

/// Read one FastCGI record from stdin: header + content + padding.
fn read_record(stdin: &InputStream) -> Option<(FcgiHeader, Vec<u8>)> {
    let header_bytes = read_exact(stdin, FCGI_HEADER_LEN)?;
    let header = FcgiHeader::parse(header_bytes[..FCGI_HEADER_LEN].try_into().ok()?);

    let content = if header.content_length > 0 {
        read_exact(stdin, header.content_length as usize)?
    } else {
        Vec::new()
    };

    // Consume padding.
    if header.padding_length > 0 {
        read_exact(stdin, header.padding_length as usize)?;
    }

    Some((header, content))
}

/// Write all bytes to stdout, handling partial writes.
fn write_all(stdout: &OutputStream, data: &[u8]) {
    let mut offset = 0;
    while offset < data.len() {
        let pollable = stdout.subscribe();
        pollable.block();
        match stdout.check_write() {
            Ok(n) if n > 0 => {
                let end = std::cmp::min(offset + n as usize, data.len());
                stdout.write(&data[offset..end]).unwrap();
                offset = end;
            }
            _ => break,
        }
    }
}

/// Write one FastCGI record to stdout.
fn write_record(stdout: &OutputStream, record_type: u8, request_id: u16, content: &[u8]) {
    // Content can be at most 65535 bytes per record. Split if needed.
    let chunks = if content.is_empty() {
        // Empty record (e.g., empty FCGI_STDOUT to signal end of stream).
        vec![&[][..]]
    } else {
        content.chunks(65535).collect()
    };

    for chunk in chunks {
        let header = FcgiHeader {
            version: FCGI_VERSION_1,
            record_type,
            request_id,
            content_length: chunk.len() as u16,
            padding_length: 0,
        };
        write_all(stdout, &header.encode());
        if !chunk.is_empty() {
            write_all(stdout, chunk);
        }
    }
}

// ---------------------------------------------------------------------------
// Application logic
// ---------------------------------------------------------------------------

struct CounterCell;

impl Guest for CounterCell {
    fn run() -> Result<(), ()> {
        let stdin = get_stdin();
        let stdout = get_stdout();

        // 1. Read FCGI_BEGIN_REQUEST.
        let (begin_hdr, begin_body) = read_record(&stdin).ok_or(())?;
        if begin_hdr.record_type != FCGI_BEGIN_REQUEST || begin_body.len() < 8 {
            return Err(());
        }
        let request_id = begin_hdr.request_id;
        let role = u16::from_be_bytes([begin_body[0], begin_body[1]]);

        if role != FCGI_RESPONDER {
            // We only support the Responder role.
            send_end_request(&stdout, request_id, 0, FCGI_UNKNOWN_ROLE);
            return Ok(());
        }

        // 2. Read FCGI_PARAMS records until an empty one.
        let mut params_buf = Vec::new();
        loop {
            let (hdr, content) = read_record(&stdin).ok_or(())?;
            if hdr.record_type != FCGI_PARAMS {
                break; // unexpected record type, stop reading params
            }
            if content.is_empty() {
                break; // empty FCGI_PARAMS marks end of params
            }
            params_buf.extend_from_slice(&content);
        }
        let params = decode_params(&params_buf);

        // 3. Read FCGI_STDIN records until an empty one (request body).
        let mut body = Vec::new();
        loop {
            let (hdr, content) = read_record(&stdin).ok_or(())?;
            if hdr.record_type != FCGI_STDIN {
                break;
            }
            if content.is_empty() {
                break; // empty FCGI_STDIN marks end of body
            }
            body.extend_from_slice(&content);
        }

        // 4. Extract REQUEST_METHOD from params.
        let method = params
            .iter()
            .find(|(k, _)| k == b"REQUEST_METHOD")
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default();

        // 5. Handle the request.
        //    Mode A: one cell per request. Counter always starts at 0.
        let count: u64 = 0;

        let (status, response_body) = match method.as_str() {
            "GET" => ("200 OK", count.to_string()),
            "POST" => ("200 OK", (count + 1).to_string()),
            _ => ("405 Method Not Allowed", "Method Not Allowed".to_string()),
        };

        // 6. Send FCGI_STDOUT: CGI response (Status header + body).
        let cgi_response = format!(
            "Status: {status}\r\nContent-Type: text/plain\r\n\r\n{response_body}"
        );
        write_record(&stdout, FCGI_STDOUT, request_id, cgi_response.as_bytes());
        // Empty FCGI_STDOUT signals end of output.
        write_record(&stdout, FCGI_STDOUT, request_id, &[]);

        // 7. Send FCGI_END_REQUEST.
        send_end_request(&stdout, request_id, 0, FCGI_REQUEST_COMPLETE);

        let _ = stdout.flush();
        Ok(())
    }
}

/// Send an FCGI_END_REQUEST record.
fn send_end_request(stdout: &OutputStream, request_id: u16, app_status: u32, protocol_status: u8) {
    let status_bytes = app_status.to_be_bytes();
    let body = [
        status_bytes[0],
        status_bytes[1],
        status_bytes[2],
        status_bytes[3],
        protocol_status,
        0, 0, 0, // reserved
    ];
    write_record(stdout, FCGI_END_REQUEST, request_id, &body);
}

wasip2::cli::command::export!(CounterCell);
