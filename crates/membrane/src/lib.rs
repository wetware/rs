//! Epoch-scoped capability primitives over Cap'n Proto RPC.
//!
//! - **Epoch** -- a monotonic sequence number anchored to on-chain state
//! - **EpochGuard** -- checks whether a capability's epoch is still current
//! - **MembraneServer** -- generic server that issues epoch-scoped sessions via `graft()`
//! - **SessionExtensionBuilder** -- trait for injecting domain-specific capabilities into sessions

#[allow(unused_parens)]
pub mod stem_capnp {
    include!(concat!(env!("OUT_DIR"), "/capnp/stem_capnp.rs"));
}

pub mod epoch;
pub mod membrane;

pub use epoch::{Epoch, EpochGuard, fill_epoch_builder};
pub use membrane::{
    membrane_client, MembraneServer, NoExtension, SessionExtensionBuilder, StatusPollerServer,
};
