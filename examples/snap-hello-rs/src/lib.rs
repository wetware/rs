//! snap-hello-rs — Farcaster Snap POC hosted on wetware.
//!
//! Snap with HTTP content negotiation + JFS-verified viewer awareness:
//!   - `Accept: application/vnd.farcaster.snap+json` → snap-JSON
//!   - anything else                                  → HTML + Link rel=alternate
//!   - `X_SNAP_FID_CLAIMED` env var (from listener)   → "Hello, FID #N"
//!     otherwise                                       → "Hello, @stranger"
//!
//! POST requests are acknowledged with the same UI tree (snap spec's
//! submit action expects the server to return next state).
//!
//! Spec: https://docs.farcaster.xyz/snap/spec-overview
//!       https://docs.farcaster.xyz/snap/http-headers
//!       https://docs.farcaster.xyz/snap/auth
//!
//! Stateless. Fresh cell per request. No graft caps used.
//!
//! IMPORTANT — FID trust model: `X_SNAP_FID_CLAIMED` is
//! cryptographically signed by the embedded JFS key, but the
//! key↔FID binding is NOT Hub-verified in v1.0 of the wetware
//! listener (see `src/jfs.rs` module docs). Cells that grant
//! authority based on FID identity SHOULD wait for v1.1. This
//! demo's "Hello, FID #N" doesn't grant authority, so claimed
//! identity is fine.

use wagi_guest as wagi;
use wasip2::exports::cli::run::Guest;

const SNAP_TYPE: &str = "application/vnd.farcaster.snap+json";

/// Render the viewer's greeting from JFS-verified env vars.
///
/// Reads `X_SNAP_FID_CLAIMED` set by the listener after JFS verification.
/// Returns `"FID #<n>"` when present, else `"@stranger"`. Falling back
/// to anonymous on missing env var matches the spec's anonymous-GET
/// posture and keeps the demo working when the listener has no JFS
/// payload to hand the cell.
fn viewer_greeting() -> String {
    match std::env::var("X_SNAP_FID_CLAIMED") {
        Ok(fid) if !fid.is_empty() => format!("FID #{fid}"),
        _ => "@stranger".to_string(),
    }
}

/// Build the HTML fallback body, splicing in the viewer greeting so
/// the OG title + h1 reflect verified identity when available.
fn html_body(greeting: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>Hello from a wetware snap</title>
  <link rel="alternate" type="application/vnd.farcaster.snap+json" href="">
  <meta property="og:title" content="Hello from a wetware snap">
</head>
<body>
  <h1>Hello, {greeting}</h1>
  <p>This is a Farcaster Snap hosted on wetware.
     Open this URL in a Farcaster client to render it.</p>
</body>
</html>"#
    )
}

/// Build the snap-JSON body for a given viewer greeting. Schema per
/// `/snap/spec-overview` + `/snap/elements`. `text.content` is
/// required, max 320 chars, `version` is the literal `"2.0"`.
fn snap_json(greeting: &str) -> String {
    serde_json::json!({
        "version": "2.0",
        "ui": {
            "root": "greeting",
            "elements": {
                "greeting": {
                    "type": "text",
                    "props": { "content": format!("Hello, {greeting}") }
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
        let greeting = viewer_greeting();

        // POST requests are submit acknowledgements — return the same
        // UI tree (snap spec's POST contract: server returns the next
        // UI state). v1 just acks; v1+ would mutate state from inputs.
        // Matches snap-aware (Accept) → snap JSON; otherwise falls
        // through to the HTML branch since a non-snap-aware POST is
        // weird but harmless.
        let method = wagi::method();
        let is_post = method == "POST";

        // `respond_bytes` flushes explicitly; plain `respond` uses
        // `print!` and can lose buffered bytes on cell teardown
        // (the body sits in stdout while only the headers ship).
        // Same fix the std/status cell uses (std/status/src/lib.rs:151-153).
        if wants_snap(&accept) || is_post {
            let body = snap_json(&greeting);
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
            let body = html_body(&greeting);
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
                body.as_bytes(),
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
    fn snap_json_anonymous_required_fields_present() {
        let body = snap_json("@stranger");
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
    fn snap_json_viewer_aware_renders_fid() {
        let body = snap_json("FID #12345");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["ui"]["elements"]["greeting"]["props"]["content"],
            "Hello, FID #12345"
        );
    }

    #[test]
    fn snap_json_text_content_under_320_chars() {
        // Use a realistic-worst-case greeting (a u64 FID maxes out at
        // 20 decimal digits + "FID #" prefix = 25 chars, plus "Hello, "
        // = 32 chars total — well under 320).
        let body = snap_json("FID #18446744073709551615");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let content = v["ui"]["elements"]["greeting"]["props"]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.chars().count() <= 320,
            "text element content must be <=320 chars per spec, got {}",
            content.chars().count()
        );
    }

    #[test]
    fn html_body_anonymous_includes_stranger() {
        let body = html_body("@stranger");
        assert!(body.contains("<h1>Hello, @stranger</h1>"));
        assert!(body.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn html_body_viewer_aware_includes_fid() {
        let body = html_body("FID #42");
        assert!(body.contains("<h1>Hello, FID #42</h1>"));
    }
}
