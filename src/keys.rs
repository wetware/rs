//! secp256k1 key management for wetware hosts.
//!
//! A single secp256k1 keypair serves as the node's unified identity:
//! - libp2p PeerId (via libp2p's secp256k1 support)
//! - Ethereum / Monad address for on-chain operations
//! - Membrane Signer for epoch-scoped session authentication
//!
//! Keys are stored as raw lowercase hex (32 bytes = 64 hex chars) on the
//! local filesystem. No external services are involved in key storage.
#![cfg(not(target_arch = "wasm32"))]

use anyhow::{bail, Context, Result};
use k256::ecdsa::SigningKey;
use libp2p::identity::Keypair;
use sha3::{Digest, Keccak256};

/// Generate a new random secp256k1 signing key using the OS CSPRNG.
pub fn generate() -> Result<SigningKey> {
    use rand::TryRngCore;
    let mut secret_bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut secret_bytes)
        .context("OS CSPRNG failed")?;
    SigningKey::from_bytes((&secret_bytes).into())
        .context("secp256k1 key generation failed (CSPRNG produced invalid scalar)")
}

/// Convert a k256 [`SigningKey`] into a libp2p [`Keypair`] (secp256k1).
///
/// The resulting keypair can be used directly with [`SwarmBuilder::with_existing_identity`].
pub fn to_libp2p(sk: &SigningKey) -> Result<Keypair> {
    let secret = libp2p::identity::secp256k1::SecretKey::try_from_bytes(sk.to_bytes().to_vec())
        .context("failed to convert secp256k1 key to libp2p identity")?;
    let kp = libp2p::identity::secp256k1::Keypair::from(secret);
    Ok(Keypair::from(kp))
}

/// Derive the Ethereum address from a secp256k1 signing key.
///
/// Returns the last 20 bytes of `Keccak256(uncompressed_pubkey[1..])`,
/// which is the standard EVM / Monad address derivation.
pub fn ethereum_address(sk: &SigningKey) -> [u8; 20] {
    let vk = sk.verifying_key();
    let uncompressed = vk.to_encoded_point(false); // 65 bytes: 0x04 || x || y
    let pubkey_bytes = &uncompressed.as_bytes()[1..]; // drop the 0x04 prefix
    let hash = Keccak256::digest(pubkey_bytes);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    addr
}

/// Load a hex-encoded secp256k1 private key from a local filesystem path.
///
/// The file must contain exactly 64 lowercase hex characters (32 bytes),
/// optionally surrounded by whitespace (newlines are trimmed).
pub fn load(path: &str) -> Result<SigningKey> {
    let hex_str =
        std::fs::read_to_string(path).with_context(|| format!("read key file: {path}"))?;

    let bytes = hex::decode(hex_str.trim())
        .context("key file must contain a hex-encoded 32-byte secp256k1 secret")?;

    if bytes.len() != 32 {
        bail!("expected 32-byte key, got {} bytes", bytes.len());
    }

    SigningKey::from_bytes(bytes.as_slice().into()).context("invalid secp256k1 secret key bytes")
}

/// Write a hex-encoded secp256k1 private key to disk.
///
/// Parent directories are created as needed. The file is written as 64
/// lowercase hex characters with no trailing newline.
pub fn save(sk: &SigningKey, path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create key directory: {}", parent.display()))?;
    }
    let hex = hex::encode(sk.to_bytes());
    std::fs::write(path, hex).with_context(|| format!("write key: {}", path.display()))
}
