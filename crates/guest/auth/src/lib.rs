//! Shared protocol types for wetware host and guest.
//!
//! Zero-dependency crate usable from both the host binary and WASM guests.

/// Signing domain for libp2p `SignedEnvelope` operations.
///
/// New domains must be added here explicitly, keeping the set finite and auditable.
///
/// # Wire format
///
/// The domain string is carried over Cap'n Proto RPC as `Text` (UTF-8).
/// Use [`SigningDomain::as_str`] to get the canonical wire form and
/// [`SigningDomain::parse`] to validate an incoming domain string.
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

    /// Construct the domain-separated signing buffer for the given payload.
    ///
    /// Format follows libp2p signed-envelope (RFC 0002):
    ///
    /// ```text
    /// varint(domain_len) domain varint(payload_type_len) payload_type varint(payload_len) payload
    /// ```
    ///
    /// Both the kernel signer and the host verifier must produce identical
    /// buffers for the same `(domain, payload)` pair.
    pub fn signing_buffer(self, payload: &[u8]) -> Vec<u8> {
        let domain = self.as_str().as_bytes();
        let payload_type = self.payload_type();
        let mut buf = Vec::with_capacity(
            varint_len(domain.len())
                + domain.len()
                + varint_len(payload_type.len())
                + payload_type.len()
                + varint_len(payload.len())
                + payload.len(),
        );
        push_varint(domain.len(), &mut buf);
        buf.extend_from_slice(domain);
        push_varint(payload_type.len(), &mut buf);
        buf.extend_from_slice(payload_type);
        push_varint(payload.len(), &mut buf);
        buf.extend_from_slice(payload);
        buf
    }
}

/// Encode an unsigned integer as a protobuf-style varint (LEB128).
fn push_varint(mut value: usize, buf: &mut Vec<u8>) {
    loop {
        if value < 0x80 {
            buf.push(value as u8);
            break;
        }
        buf.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
}

/// Number of bytes needed for a varint-encoded value.
fn varint_len(value: usize) -> usize {
    let mut v = value;
    let mut len = 1;
    while v >= 0x80 {
        v >>= 7;
        len += 1;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_buffer_membrane_graft_structure() {
        let nonce: u64 = 0x0102030405060708;
        let buf = SigningDomain::MembraneGraft.signing_buffer(&nonce.to_be_bytes());

        // Expected layout:
        //   varint(17) "ww-membrane-graft"     (17 bytes)
        //   varint(24) "/ww/membrane/graft-nonce"  (24 bytes)
        //   varint(8)  <8-byte nonce>
        let domain = b"ww-membrane-graft";
        let payload_type = b"/ww/membrane/graft-nonce";
        assert_eq!(domain.len(), 17);
        assert_eq!(payload_type.len(), 24);

        let mut expected = Vec::new();
        expected.push(17u8); // varint(17)
        expected.extend_from_slice(domain);
        expected.push(24u8); // varint(24)
        expected.extend_from_slice(payload_type);
        expected.push(8u8); // varint(8)
        expected.extend_from_slice(&nonce.to_be_bytes());

        assert_eq!(buf, expected);
    }

    #[test]
    fn signing_buffer_deterministic() {
        let payload = b"test-payload";
        let a = SigningDomain::MembraneGraft.signing_buffer(payload);
        let b = SigningDomain::MembraneGraft.signing_buffer(payload);
        assert_eq!(a, b, "same inputs must produce identical buffers");
    }

    #[test]
    fn varint_single_byte() {
        let mut buf = Vec::new();
        push_varint(0, &mut buf);
        assert_eq!(buf, vec![0]);

        buf.clear();
        push_varint(127, &mut buf);
        assert_eq!(buf, vec![127]);
    }

    #[test]
    fn varint_multi_byte() {
        let mut buf = Vec::new();
        push_varint(128, &mut buf);
        assert_eq!(buf, vec![0x80, 0x01]);

        buf.clear();
        push_varint(300, &mut buf);
        // 300 = 0b100101100 → 0b0101100 | 0x80, 0b10 → [0xAC, 0x02]
        assert_eq!(buf, vec![0xAC, 0x02]);
    }
}
