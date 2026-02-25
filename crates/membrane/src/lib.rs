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
pub mod ipfs_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/ipfs_capnp.rs"));
}

#[allow(
    unused_parens,
    clippy::extra_unused_type_parameters,
    clippy::match_single_binding
)]
pub mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/stem_capnp.rs"));
}

pub mod epoch;
pub mod membrane;

pub use epoch::{Epoch, EpochGuard};
pub use membrane::{membrane_client, GraftBuilder, MembraneServer, NoExtension};
