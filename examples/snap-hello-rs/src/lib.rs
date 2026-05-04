//! snap-hello-rs — Farcaster Snap POC hosted on wetware.
//!
//! Static `Hello, @stranger` snap with HTTP content negotiation:
//!   - `Accept: application/vnd.farcaster.snap+json` → snap-JSON
//!   - anything else                                  → HTML + Link rel=alternate
//!
//! Spec: https://docs.farcaster.xyz/snap/spec-overview
//!       https://docs.farcaster.xyz/snap/http-headers
//!
//! Stateless. Fresh cell per request. No graft caps used.

use wagi_guest as wagi;
use wasip2::exports::cli::run::Guest;

const SNAP_TYPE: &str = "application/vnd.farcaster.snap+json";

/// Minimal HTML fallback for non-Farcaster visitors. The `Link` header
/// (set in the response) does the real protocol-discovery work; the body
/// is just a friendly placeholder for browsers, link previewers, and
/// crawlers.
const HTML_BODY: &str = r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>Hello from a wetware snap</title>
  <link rel="alternate" type="application/vnd.farcaster.snap+json" href="">
  <meta property="og:title" content="Hello from a wetware snap">
</head>
<body>
  <h1>Hello, @stranger</h1>
  <p>This is a Farcaster Snap hosted on wetware.
     Open this URL in a Farcaster client to render it.</p>
</body>
</html>"#;

/// Build the snap-JSON body. Schema per `/snap/spec-overview` +
/// `/snap/elements` (verified 2026-05-02). `text` element uses
/// `content` (max 320 chars), `version` is the literal `"2.0"`.
fn snap_json() -> String {
    serde_json::json!({
        "version": "2.0",
        "ui": {
            "root": "greeting",
            "elements": {
                "greeting": {
                    "type": "text",
                    "props": { "content": "Hello, @stranger" }
                }
            }
        }
    })
    .to_string()
}

/// True when the request's `Accept` header mentions the snap media
/// type. Naive `contains` is sufficient: Farcaster clients send the
/// type explicitly per the spec, and we don't need full
/// content-negotiation q-value parsing for this POC.
fn wants_snap(accept: &str) -> bool {
    accept.contains(SNAP_TYPE)
}

struct SnapCell;

impl Guest for SnapCell {
    fn run() -> Result<(), ()> {
        let accept = wagi::header("Accept").unwrap_or_default();

        // `respond_bytes` flushes explicitly; plain `respond` uses
        // `print!` and can lose buffered bytes on cell teardown
        // (the body sits in stdout while only the headers ship).
        // Same fix the std/status cell uses (std/status/src/lib.rs:151-153).
        if wants_snap(&accept) {
            let body = snap_json();
            wagi::respond_bytes(
                200,
                &[
                    ("Content-Type", SNAP_TYPE),
                    ("Vary", "Accept"),
                    ("Cache-Control", "public, max-age=300"),
                    ("Access-Control-Allow-Origin", "*"),
                ],
                body.as_bytes(),
            );
        } else {
            // `Link: <>; rel="alternate"; type="..."` — empty `<>` is
            // an RFC 3986 same-document reference; clients re-fetch
            // the current URL with `Accept` set. Avoids hardcoding an
            // absolute URL into the cell.
            wagi::respond_bytes(
                200,
                &[
                    ("Content-Type", "text/html; charset=utf-8"),
                    ("Vary", "Accept"),
                    ("Cache-Control", "public, max-age=300"),
                    ("Access-Control-Allow-Origin", "*"),
                    (
                        "Link",
                        "<>; rel=\"alternate\"; type=\"application/vnd.farcaster.snap+json\"",
                    ),
                ],
                HTML_BODY.as_bytes(),
            );
        }

        Ok(())
    }
}

wasip2::cli::command::export!(SnapCell);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_snap_exact_match() {
        assert!(wants_snap("application/vnd.farcaster.snap+json"));
    }

    #[test]
    fn wants_snap_in_accept_list() {
        assert!(wants_snap(
            "text/html, application/vnd.farcaster.snap+json, */*"
        ));
    }

    #[test]
    fn wants_snap_html_only_returns_false() {
        assert!(!wants_snap("text/html"));
    }

    #[test]
    fn wants_snap_empty_returns_false() {
        assert!(!wants_snap(""));
    }

    #[test]
    fn snap_json_required_fields_present() {
        let body = snap_json();
        let v: serde_json::Value = serde_json::from_str(&body).expect("snap_json must parse");
        assert_eq!(v["version"], "2.0");
        assert_eq!(v["ui"]["root"], "greeting");
        assert_eq!(v["ui"]["elements"]["greeting"]["type"], "text");
        assert_eq!(
            v["ui"]["elements"]["greeting"]["props"]["content"],
            "Hello, @stranger"
        );
    }

    #[test]
    fn snap_json_text_content_under_320_chars() {
        let body = snap_json();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let content = v["ui"]["elements"]["greeting"]["props"]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.chars().count() <= 320,
            "text element content must be <=320 chars per spec"
        );
    }
}
