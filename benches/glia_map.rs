//! Benchmarks for Glia Val::Map operations.
//!
//! Measures O(N) linear scan cost for contains?, get, assoc on
//! Vec<(Val, Val)> maps. Establishes baseline for Phase 2 ValMap hybrid.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use glia::Val;

/// Build a Val::Map with N keyword-keyed entries: {:key_0 0, :key_1 1, ...}
fn make_map(n: usize) -> Val {
    let pairs: Vec<(Val, Val)> = (0..n)
        .map(|i| (Val::Keyword(format!("key_{i}")), Val::Int(i as i64)))
        .collect();
    Val::Map(pairs)
}

fn bench_map_contains(c: &mut Criterion) {
    let mut group = c.benchmark_group("map_contains_key");
    for n in [4, 8, 16, 32, 64, 128] {
        let map = make_map(n);
        // Look up the last key (worst case for linear scan)
        let needle = Val::Keyword(format!("key_{}", n - 1));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                if let Val::Map(pairs) = &map {
                    pairs.iter().any(|(k, _)| k == &needle)
                } else {
                    unreachable!()
                }
            });
        });
    }
    group.finish();
}

fn bench_map_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("map_get");
    for n in [4, 8, 16, 32, 64, 128] {
        let map = make_map(n);
        let needle = Val::Keyword(format!("key_{}", n - 1));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                if let Val::Map(pairs) = &map {
                    pairs.iter().find(|(k, _)| k == &needle).map(|(_, v)| v)
                } else {
                    unreachable!()
                }
            });
        });
    }
    group.finish();
}

fn bench_map_assoc(c: &mut Criterion) {
    let mut group = c.benchmark_group("map_assoc");
    for n in [4, 8, 16, 32, 64, 128] {
        let map = make_map(n);
        let new_key = Val::Keyword("new_key".into());
        let new_val = Val::Int(999);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                // Replicate assoc: clone map, linear search, append
                if let Val::Map(pairs) = &map {
                    let mut cloned = pairs.clone();
                    if let Some(entry) = cloned.iter_mut().find(|(k, _)| k == &new_key) {
                        entry.1 = new_val.clone();
                    } else {
                        cloned.push((new_key.clone(), new_val.clone()));
                    }
                    Val::Map(cloned)
                } else {
                    unreachable!()
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_map_contains, bench_map_get, bench_map_assoc);
criterion_main!(benches);
