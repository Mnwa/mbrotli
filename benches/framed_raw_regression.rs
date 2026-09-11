//! Fixed raw workloads for before/after framing changes, with a C oracle.
#[path = "../tests/decode_support/c_decoder.rs"]
mod c_decoder;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use mbrotli::{Compressor, Decompressor, EncoderConfig, Quality};
use std::hint::black_box;
fn benchmarks(c: &mut Criterion) {
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
        (
            "text",
            b"Brotli framing preserves existing raw decoding semantics. ".repeat(1024),
        ),
        ("binary", noise),
        ("small", b"small raw decoder overhead payload".to_vec()),
    ];
    let mut group = c.benchmark_group("framed-raw-regression");
    for (name, bytes) in corpora {
        let wire = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))
            .unwrap()
            .compress(&bytes)
            .unwrap();
        let mut decoder = Decompressor::new(Default::default()).unwrap();
        let mut output = vec![0; bytes.len()];
        decoder.decompress_to_slice(&wire, &mut output).unwrap();
        assert_eq!(output, bytes);
        let mut reference = c_decoder::Decoder::default();
        let (consumed, produced, finished) = reference.process(&wire, &mut output);
        assert!(finished);
        assert_eq!((consumed, produced), (wire.len(), bytes.len()));
        assert_eq!(output, bytes);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        eprintln!("{name}: payload={} compressed={}", bytes.len(), wire.len());
        group.bench_function(format!("{name}/rust"), |b| {
            b.iter(|| {
                decoder
                    .decompress_to_slice(black_box(&wire), black_box(&mut output))
                    .unwrap()
            })
        });
        group.bench_function(format!("{name}/c"), |b| {
            b.iter(|| {
                let mut d = c_decoder::Decoder::default();
                black_box(d.process(black_box(&wire), black_box(&mut output)));
            })
        });
    }
    group.finish();
}
criterion_group!(benches, benchmarks);
criterion_main!(benches);
