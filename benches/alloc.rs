//! Alloc-backed one-shot API comparison, available with and without std.
//!
//! Both timed calls construct an encoder and allocate their output. Corpus
//! construction, validation, and compressed-size reporting are outside timing.

#[path = "../tests/support/mod.rs"]
mod support;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mbrotli::{Compressor, EncoderConfig, Quality};
use std::hint::black_box;
use support::{c_compress, c_decompress};

fn alloc_compression(criterion: &mut Criterion) {
    let mut state = 1u32;
    let noise: Vec<u8> = (0..65536)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    let corpora = [
        ("small", b"a small Brotli payload".to_vec()),
        (
            "text",
            b"Brotli compression using core and alloc. ".repeat(1600),
        ),
        ("binary", (0..65536).map(|i| (i % 251) as u8).collect()),
        ("repeated", vec![b'a'; 65536]),
        ("noise", noise),
        (
            "large",
            b"large repeated text with a bounded alphabet ".repeat(25000),
        ),
    ];
    for quality in [Quality::Q0, Quality::Q5, Quality::Q11] {
        let config = EncoderConfig::default().with_quality(quality);
        let mut group = criterion.benchmark_group(format!("alloc/q{}", quality.get()));
        for (name, input) in &corpora {
            let rust = Compressor::new(config).unwrap().compress(input).unwrap();
            let reference = c_compress(quality.get().into(), 22, input);
            assert_eq!(rust, reference);
            assert_eq!(
                c_decompress(&rust, input.len()).as_deref(),
                Some(input.as_slice())
            );
            eprintln!(
                "q{} {name}: {} -> {} bytes (Rust and C)",
                quality.get(),
                input.len(),
                rust.len()
            );
            group.throughput(Throughput::Bytes(input.len() as u64));
            group.bench_with_input(BenchmarkId::new("rust", name), input, |b, input| {
                b.iter(|| {
                    Compressor::new(black_box(config))
                        .unwrap()
                        .compress(black_box(input))
                        .unwrap()
                });
            });
            group.bench_with_input(BenchmarkId::new("c", name), input, |b, input| {
                b.iter(|| c_compress(black_box(quality.get().into()), 22, black_box(input)));
            });
        }
        group.finish();
    }
}

criterion_group!(benches, alloc_compression);
criterion_main!(benches);
