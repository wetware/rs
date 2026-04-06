//! Benchmarks for Glia Val::Map operations.
//!
//! Measures ValMap hybrid (Vec for small, im::HashMap for large) performance
//! for contains_key, get, and assoc across map sizes.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use glia::{Val, ValMap};

/// Build a Val::Map with N keyword-keyed entries: {:key_0 0, :key_1 1, ...}
fn make_map(n: usize) -> Val {
    let pairs: Vec<(Val, Val)> = (0..n)
        .map(|i| (Val::Keyword(format!("key_{i}")), Val::Int(i as i64)))
        .collect();
    Val::Map(ValMap::from_pairs(pairs))
}

fn bench_map_contains(c: &mut Criterion) {
    let mut group = c.benchmark_group("map_contains_key");
    for n in [4, 8, 16, 32, 64, 128] {
        let map = make_map(n);
        let needle = Val::Keyword(format!("key_{}", n - 1));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                if let Val::Map(m) = &map {
                    m.contains_key(&needle)
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
                if let Val::Map(m) = &map {
                    m.get(&needle)
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
                if let Val::Map(m) = &map {
                    Val::Map(m.assoc(new_key.clone(), new_val.clone()))
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
