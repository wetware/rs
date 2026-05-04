//! Ed25519 key management for wetware hosts.
//!
//! A single Ed25519 keypair serves as the node's identity:
//! - libp2p PeerId (via libp2p's ed25519 support)
//! - Membrane Signer for epoch-scoped session authentication
//!
//! Operator identity (secp256k1 for on-chain Stem contract ownership) is a
//! separate concern managed outside the node runtime.
//!
//! Keys are stored as base58btc (Bitcoin alphabet, ~44 chars for 32 bytes)
//! on the local filesystem, aligning with the libp2p ecosystem.
#![cfg(not(target_arch = "wasm32"))]

use anyhow::{bail, Context, Result};
use base58::{FromBase58, ToBase58};
use ed25519_dalek::SigningKey;
use libp2p::identity::Keypair;

/// Generate a new random Ed25519 signing key using the OS CSPRNG.
pub fn generate() -> Result<SigningKey> {
    use rand::TryRngCore;
    let mut secret_bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut secret_bytes)
        .context("OS CSPRNG failed")?;
    Ok(SigningKey::from_bytes(&secret_bytes))
}

/// Encode a signing key as a base58btc string.
pub fn encode(sk: &SigningKey) -> String {
    sk.to_bytes().to_base58()
}

/// Convert an Ed25519 [`SigningKey`] into a libp2p [`Keypair`].
///
/// The resulting keypair can be used directly with [`SwarmBuilder::with_existing_identity`].
pub fn to_libp2p(sk: &SigningKey) -> Result<Keypair> {
    let kp = libp2p::identity::ed25519::Keypair::try_from_bytes(&mut sk.to_keypair_bytes())
        .context("failed to convert Ed25519 key to libp2p identity")?;
    Ok(Keypair::from(kp))
}

/// Decode a base58btc string into a 32-byte signing key.
fn decode(s: &str) -> Result<SigningKey> {
    let bytes = s
        .from_base58()
        .map_err(|_| anyhow::anyhow!("key must be base58btc-encoded (~44 chars)"))?;

    if bytes.len() != 32 {
        bail!("expected 32-byte key, got {} bytes", bytes.len());
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&arr))
}

/// Load an Ed25519 private key from a local filesystem path (base58btc).
pub fn load(path: &str) -> Result<SigningKey> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read key file: {path}"))?;
    decode(contents.trim()).with_context(|| format!("invalid key in {path}"))
}

/// Write a base58btc-encoded Ed25519 private key to disk.
///
/// Parent directories are created as needed.
pub fn save(sk: &SigningKey, path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create key directory: {}", parent.display()))?;
    }
    std::fs::write(path, encode(sk)).with_context(|| format!("write key: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_base58() {
        let sk = generate().unwrap();
        let encoded = encode(&sk);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(sk.to_bytes(), decoded.to_bytes());
    }

    #[test]
    fn hex_encoding_rejected() {
        let sk = generate().unwrap();
        let hex_str = hex::encode(sk.to_bytes());
        // Hex is not accepted — base58btc only.
        assert!(decode(&hex_str).is_err());
    }

    #[test]
    fn base58_is_shorter_than_hex() {
        let sk = generate().unwrap();
        let b58 = encode(&sk);
        let hex_str = hex::encode(sk.to_bytes());
        assert!(
            b58.len() < hex_str.len(),
            "base58 ({}) should be shorter than hex ({})",
            b58.len(),
            hex_str.len()
        );
    }

    #[test]
    fn invalid_encoding_rejected() {
        assert!(decode("not-valid-anything!!!").is_err());
    }

    #[test]
    fn wrong_length_rejected() {
        let short = [1u8; 16].to_base58();
        assert!(decode(&short).is_err());
    }
}
