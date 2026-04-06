use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use cid::Cid;

/// Lock-free probabilistic set membership using atomic bit arrays.
///
/// False positives are possible (configurable via `fpr`), false negatives are not.
/// All operations use `Relaxed` ordering -- sufficient because we only ever set bits
/// (monotonic), and a stale read simply causes a redundant ARC lookup.
pub struct AtomicBloom {
    bits: Box<[AtomicU64]>,
    num_hashes: u32,
    total_bits: u64,
}

impl AtomicBloom {
    /// Create a new bloom filter sized for `capacity` elements at a
    /// false-positive rate of `fpr` (e.g. 0.00001 = 0.001%).
    pub fn new(capacity: usize, fpr: f64) -> Self {
        // Optimal bit count: m = -n * ln(fpr) / ln(2)^2
        let m = -(capacity as f64) * fpr.ln() / (2.0_f64.ln().powi(2));
        // Round up to next multiple of 64.
        let m = (m as u64).div_ceil(64) * 64;

        // Optimal hash count: k = (m/n) * ln(2)
        let k = ((m as f64 / capacity as f64) * 2.0_f64.ln()).round() as u32;
        let k = k.max(1);

        let words = (m / 64) as usize;
        let bits: Vec<AtomicU64> = (0..words).map(|_| AtomicU64::new(0)).collect();

        Self {
            bits: bits.into_boxed_slice(),
            num_hashes: k,
            total_bits: m,
        }
    }

    /// Record a CID as present. Lock-free via atomic fetch_or.
    pub fn insert(&self, cid: &Cid) {
        let (h1, h2) = self.double_hash(cid);
        for i in 0..self.num_hashes {
            let bit = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.total_bits;
            let word = (bit / 64) as usize;
            let offset = bit % 64;
            self.bits[word].fetch_or(1 << offset, Ordering::Relaxed);
        }
    }

    /// Check if a CID was probably inserted. False positives possible,
    /// false negatives impossible.
    pub fn probably_contains(&self, cid: &Cid) -> bool {
        let (h1, h2) = self.double_hash(cid);
        for i in 0..self.num_hashes {
            let bit = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.total_bits;
            let word = (bit / 64) as usize;
            let offset = bit % 64;
            if self.bits[word].load(Ordering::Relaxed) & (1 << offset) == 0 {
                return false;
            }
        }
        true
    }

    /// Double-hashing: hash the CID once, then rehash to get a second
    /// independent hash value.
    fn double_hash(&self, cid: &Cid) -> (u64, u64) {
        let mut hasher = DefaultHasher::new();
        // Cid::hash() returns the multihash, not the Hash trait.
        // Hash the CID's byte representation instead.
        cid.to_bytes().hash(&mut hasher);
        let h = hasher.finish();

        // Second hash: rehash the first to get an independent value.
        let mut hasher2 = DefaultHasher::new();
        h.hash(&mut hasher2);
        let h2 = hasher2.finish();

        // Ensure h2 is odd to guarantee full period over the bit space.
        (h, h2 | 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cid(seed: u64) -> Cid {
        let mh =
            cid::multihash::Multihash::wrap(0x00, &seed.to_le_bytes()).expect("identity multihash");
        Cid::new_v1(0x55, mh)
    }

    #[test]
    fn test_insert_and_contains() {
        let bloom = AtomicBloom::new(10_000, 0.00001);
        for i in 0..100 {
            bloom.insert(&make_cid(i));
        }
        for i in 0..100 {
            assert!(
                bloom.probably_contains(&make_cid(i)),
                "inserted CID {} should be found",
                i
            );
        }
    }

    #[test]
    fn test_definitely_absent() {
        let bloom = AtomicBloom::new(10_000, 0.00001);
        for i in 0..100 {
            assert!(
                !bloom.probably_contains(&make_cid(i)),
                "empty bloom should not contain CID {}",
                i
            );
        }
    }

    #[test]
    fn test_false_positive_rate() {
        let bloom = AtomicBloom::new(100_000, 0.00001);

        for i in 0..10_000u64 {
            bloom.insert(&make_cid(i));
        }

        let false_positives = (1_000_000..1_010_000u64)
            .filter(|&i| bloom.probably_contains(&make_cid(i)))
            .count();

        let fpr = false_positives as f64 / 10_000.0;
        assert!(
            fpr < 0.01,
            "false positive rate {:.4} exceeds generous 1% threshold",
            fpr
        );
    }

    #[test]
    fn test_concurrent_insert() {
        use std::sync::Arc;
        use std::thread;

        let bloom = Arc::new(AtomicBloom::new(100_000, 0.00001));

        let mut handles = Vec::new();
        for t in 0..4u64 {
            let bloom = Arc::clone(&bloom);
            handles.push(thread::spawn(move || {
                let base = t * 1000;
                for i in 0..1000 {
                    bloom.insert(&make_cid(base + i));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        for t in 0..4u64 {
            let base = t * 1000;
            for i in 0..1000 {
                assert!(
                    bloom.probably_contains(&make_cid(base + i)),
                    "CID {} (thread {}) missing after concurrent insert",
                    base + i,
                    t
                );
            }
        }
    }
}
