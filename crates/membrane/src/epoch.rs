//! Epoch types and the epoch validity guard.

use capnp::Error;
use tokio::sync::watch;

/// Epoch value used by the membrane (matches capnp struct Epoch).
#[derive(Clone, Debug)]
pub struct Epoch {
    pub seq: u64,
    pub head: Vec<u8>,
    pub adopted_block: u64,
}

/// Guard that checks whether the epoch under which a capability was issued is
/// still current. Shared by all session-scoped capability servers so that
/// every RPC hard-fails once the epoch advances.
#[derive(Clone)]
pub struct EpochGuard {
    pub issued_seq: u64,
    pub receiver: watch::Receiver<Epoch>,
}

impl EpochGuard {
    pub fn check(&self) -> Result<(), Error> {
        let current = self.receiver.borrow();
        if current.seq != self.issued_seq {
            return Err(Error::failed(
                "staleEpoch: session epoch no longer current".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch(seq: u64, head: &[u8], adopted_block: u64) -> Epoch {
        Epoch {
            seq,
            head: head.to_vec(),
            adopted_block,
        }
    }

    #[tokio::test]
    async fn epoch_guard_ok_when_seq_matches() {
        let (_tx, rx) = watch::channel(epoch(1, b"head1", 100));
        let guard = EpochGuard {
            issued_seq: 1,
            receiver: rx,
        };
        assert!(guard.check().is_ok());
    }

    #[tokio::test]
    async fn epoch_guard_fails_when_seq_differs() {
        let (tx, rx) = watch::channel(epoch(1, b"head1", 100));
        let guard = EpochGuard {
            issued_seq: 1,
            receiver: rx,
        };
        assert!(guard.check().is_ok());
        tx.send(epoch(2, b"head2", 101)).unwrap();
        let res = guard.check();
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("staleEpoch"));
    }
}
