//! Benchmarks for the ARC cache (crates/cache/src/arc.rs).
//!
//! Measures get (hit/miss), put (with/without eviction), and mixed workloads.
//! Establishes baseline for cache performance monitoring.

use cache::ArcInner;
use cid::Cid;
use criterion::{criterion_group, criterion_main, Criterion};

/// Create a deterministic CID from an integer seed using raw multihash bytes.
fn make_cid(seed: u64) -> Cid {
    // Build a CID with identity multihash (0x00) containing the seed bytes.
    // This is fast and deterministic — good enough for benchmarking.
    let digest =
        cid::multihash::Multihash::wrap(0x00, &seed.to_le_bytes()).expect("identity multihash");
    Cid::new_v1(0x55, digest) // raw codec
}

/// Unit weighter: each entry weighs 1.
fn unit_weighter(_k: &Cid, _v: &Vec<u8>) -> usize {
    1
}

fn bench_arc_get_hit(c: &mut Criterion) {
    let mut cache = ArcInner::new(1000, 256, unit_weighter);
    // Fill cache to 80%
    for i in 0..800u64 {
        cache.put(make_cid(i), vec![i as u8; 64]);
    }
    let target = make_cid(400); // known to be in cache
    c.bench_function("arc_get_hit", |b| {
        b.iter(|| {
            cache.get(&target);
        });
    });
}

fn bench_arc_get_miss(c: &mut Criterion) {
    let mut cache = ArcInner::new(1000, 256, unit_weighter);
    for i in 0..800u64 {
        cache.put(make_cid(i), vec![i as u8; 64]);
    }
    let miss_key = make_cid(9999); // not in cache
    c.bench_function("arc_get_miss", |b| {
        b.iter(|| {
            cache.get(&miss_key);
        });
    });
}

fn bench_arc_put_no_eviction(c: &mut Criterion) {
    let mut cache = ArcInner::new(10_000, 256, unit_weighter);
    let mut seed = 0u64;
    c.bench_function("arc_put_no_eviction", |b| {
        b.iter(|| {
            seed += 1;
            cache.put(make_cid(seed), vec![0u8; 64]);
        });
    });
}

fn bench_arc_put_with_eviction(c: &mut Criterion) {
    let mut cache = ArcInner::new(100, 64, unit_weighter);
    // Fill to capacity
    for i in 0..100u64 {
        cache.put(make_cid(i), vec![i as u8; 64]);
    }
    let mut seed = 1000u64;
    c.bench_function("arc_put_with_eviction", |b| {
        b.iter(|| {
            seed += 1;
            let evicted = cache.put(make_cid(seed), vec![0u8; 64]);
            drop(evicted);
        });
    });
}

criterion_group!(
    benches,
    bench_arc_get_hit,
    bench_arc_get_miss,
    bench_arc_put_no_eviction,
    bench_arc_put_with_eviction,
);
criterion_main!(benches);
