//! Epoch-scoped capability primitives over Cap'n Proto RPC.
//!
//! - **Epoch** -- a monotonic sequence number anchored to on-chain state
//! - **EpochGuard** -- checks whether a capability's epoch is still current
//! - **MembraneServer** -- server that issues epoch-scoped sessions via `graft()`
//! - **SessionBuilder** -- trait for injecting domain-specific capabilities into sessions

#[allow(unused_parens, clippy::match_single_binding)]
pub mod system_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/system_capnp.rs"));
}

#[allow(unused_parens, clippy::match_single_binding)]
pub mod routing_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/routing_capnp.rs"));
}

#[allow(
    unused_parens,
    clippy::extra_unused_type_parameters,
    clippy::match_single_binding
)]
pub mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/stem_capnp.rs"));
}

// cell_capnp is still needed by the host for Raw/Http cell type decoding.
// TODO: remove once Raw/Http cells migrate to envvars.
#[allow(unused_parens, clippy::match_single_binding)]
pub mod cell_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/cell_capnp.rs"));
}

#[allow(unused_parens, clippy::match_single_binding)]
pub mod http_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/http_capnp.rs"));
}

/// Canonical Schema.Node bytes for each grafted capability interface.
/// Populated into `Export.schema` at graft time so guests can introspect
/// the interface without hardcoded descriptions. See `build.rs`.
pub mod schema_registry {
    include!(concat!(env!("OUT_DIR"), "/schema_ids.rs"));

    /// Resolve canonical Schema.Node bytes by the canonical cap name used
    /// in the membrane graft loop (e.g. "host", "runtime", "routing",
    /// "identity", "http-client"). Returns `None` for unknown names so
    /// callers can fall back to an empty schema rather than panicking.
    pub fn schema_by_name(name: &str) -> Option<&'static [u8]> {
        match name {
            "host" => Some(HOST_SCHEMA),
            "runtime" => Some(RUNTIME_SCHEMA),
            "routing" => Some(ROUTING_SCHEMA),
            "identity" => Some(IDENTITY_SCHEMA),
            "http-client" => Some(HTTP_CLIENT_SCHEMA),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn each_core_cap_has_non_empty_bytes() {
            for name in ["host", "runtime", "routing", "identity", "http-client"] {
                let bytes = schema_by_name(name)
                    .unwrap_or_else(|| panic!("missing schema for cap '{name}'"));
                assert!(!bytes.is_empty(), "schema for '{name}' is empty");
            }
        }

        #[test]
        fn unknown_cap_returns_none() {
            assert!(schema_by_name("nonexistent").is_none());
            assert!(schema_by_name("").is_none());
        }

        #[test]
        fn bytes_are_word_aligned() {
            for name in ["host", "runtime", "routing", "identity", "http-client"] {
                let bytes = schema_by_name(name).expect("schema present");
                assert_eq!(
                    bytes.len() % 8,
                    0,
                    "canonical schema for '{name}' must be word-aligned (got {} bytes)",
                    bytes.len()
                );
            }
        }

        #[test]
        fn bytes_parse_as_schema_node() {
            for name in ["host", "runtime", "routing", "identity", "http-client"] {
                let bytes = schema_by_name(name).expect("schema present");
                // Capnp segments require 8-byte alignment; the static byte
                // slice is only byte-aligned, so copy into a Word buffer.
                let word_count = bytes.len().div_ceil(8);
                let mut words: Vec<capnp::Word> =
                    vec![capnp::word(0, 0, 0, 0, 0, 0, 0, 0); word_count];
                capnp::Word::words_to_bytes_mut(&mut words)[..bytes.len()].copy_from_slice(bytes);
                let aligned = capnp::Word::words_to_bytes(&words);
                let segments: &[&[u8]] = &[aligned];
                let segment_array = capnp::message::SegmentArray::new(segments);
                let reader = capnp::message::Reader::new(
                    segment_array,
                    capnp::message::ReaderOptions::new(),
                );
                let node: capnp::schema_capnp::node::Reader =
                    reader.get_root().expect("root is a node");
                let which = node.which().expect("node has Which");
                assert!(
                    matches!(which, capnp::schema_capnp::node::Which::Interface(_)),
                    "schema for '{name}' is not an interface node"
                );
            }
        }
    }
}

pub mod epoch;
pub mod membrane;
pub mod terminal;

pub use epoch::{Epoch, EpochGuard, Provenance};
pub use membrane::{membrane_client, GraftBuilder, MembraneServer, NoExtension};
pub use terminal::{AllowAllPolicy, AuthPolicy, TerminalServer, VerifyingKeyPolicy};
