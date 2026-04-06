//! Benchmarks for host-guest stream I/O (src/cell/streams.rs).
//!
//! Measures Writer::poll_write and Reader::poll_read throughput
//! across chunk sizes. Establishes baseline for Phase 2 Bytes migration.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

fn bench_stream_writer(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("stream_writer");
    for chunk_size in [256, 1024, 8192, 65536] {
        group.throughput(Throughput::Bytes(chunk_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(chunk_size),
            &chunk_size,
            |b, &size| {
                b.to_async(&rt).iter(|| async move {
                    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                    let data = vec![0xABu8; size];
                    // Write side: .to_vec() per write (current pattern)
                    tx.send(data.to_vec()).unwrap();
                    // Read side: drain
                    let _ = rx.recv().await.unwrap();
                });
            },
        );
    }
    group.finish();
}

fn bench_stream_roundtrip(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("stream_roundtrip");
    for chunk_size in [256, 1024, 8192, 65536] {
        group.throughput(Throughput::Bytes(chunk_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(chunk_size),
            &chunk_size,
            |b, &size| {
                b.to_async(&rt).iter(|| async move {
                    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
                    let mut writer = ww::cell::streams::Writer::new(tx);
                    let mut reader = ww::cell::streams::Reader::new(rx);

                    let data = vec![0xABu8; size];
                    writer.write_all(&data).await.unwrap();
                    drop(writer); // signal EOF

                    let mut out = vec![0u8; size];
                    reader.read_exact(&mut out).await.unwrap();
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_stream_writer, bench_stream_roundtrip);
criterion_main!(benches);
