//! Benchmarks for kernel capability dispatch (extract_method pattern).
//!
//! The kernel crate targets wasm32, so we replicate the extract_method
//! logic inline (15 lines of pure Val manipulation) to benchmark the
//! allocation pattern. Establishes baseline for Phase 2 borrowing optimization.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use glia::Val;

/// Replicate the current extract_method: clones method String + .to_vec() on args.
fn extract_method_clone(data: &Val) -> Result<(String, Vec<Val>), Val> {
    let items = match data {
        Val::List(items) => items,
        _ => return Err(Val::from("expected list data")),
    };
    let method = match items.first() {
        Some(Val::Keyword(s)) => s.clone(),
        _ => return Err(Val::from("first arg must be a keyword")),
    };
    Ok((method, items[1..].to_vec()))
}

/// The target: borrow-based extraction (no allocation).
fn extract_method_borrow(data: &Val) -> Result<(&str, &[Val]), Val> {
    let items = match data {
        Val::List(items) => items.as_slice(),
        _ => return Err(Val::from("expected list data")),
    };
    let method = match items.first() {
        Some(Val::Keyword(s)) => s.as_str(),
        _ => return Err(Val::from("first arg must be a keyword")),
    };
    Ok((method, &items[1..]))
}

fn bench_extract_method(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_method");

    for n_args in [0, 1, 4, 8, 16] {
        // Build a Val::List with :method keyword + N args
        let mut items = vec![Val::Keyword("run".into())];
        for i in 0..n_args {
            items.push(Val::Str(format!("arg_{i}")));
        }
        let data = Val::List(items);

        group.bench_with_input(BenchmarkId::new("clone", n_args), &data, |b, data| {
            b.iter(|| extract_method_clone(data));
        });

        group.bench_with_input(BenchmarkId::new("borrow", n_args), &data, |b, data| {
            b.iter(|| extract_method_borrow(data));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_extract_method);
criterion_main!(benches);
