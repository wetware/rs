//! Shared protocol types for wetware host and guest.
//!
//! Zero-dependency crate usable from both the host binary and WASM guests.

/// Signing domain for libp2p [`SignedEnvelope`][libp2p_core::SignedEnvelope] operations.
///
/// The exhaustive match ensures the host validates incoming domain strings
/// against a closed set of known domains.  Any unknown string is rejected
/// before signing occurs.  New domains must be added here explicitly, keeping
/// the set finite and auditable.
///
/// # Wire format
///
/// The domain string is carried over Cap'n Proto RPC as `Text` (UTF-8).
/// Use [`SigningDomain::as_str`] to get the canonical wire form, and
/// [`SigningDomain::from_str`] to parse and validate an incoming domain string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigningDomain {
    /// Challenge-response domain for `Membrane::graft`.
    MembraneGraft,
}

impl SigningDomain {
    /// Canonical libp2p signed-envelope domain string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MembraneGraft => "ww-membrane-graft",
        }
    }

    /// Payload type for the libp2p signed envelope.
    pub fn payload_type(self) -> &'static [u8] {
        match self {
            Self::MembraneGraft => b"/ww/membrane/graft-nonce",
        }
    }

    /// Parse from the wire domain string.  Returns `None` for unknown domains.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ww-membrane-graft" => Some(Self::MembraneGraft),
            _ => None,
        }
    }
}
