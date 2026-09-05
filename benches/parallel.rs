//! End-to-end parallel scaling, with serial C/Rust baselines and explicit sizes.
//! Parallel and serial streams have different history policies; only parallel
//! task-count comparisons are equivalent compressed-output speed measurements.
#[path = "../tests/support/mod.rs"]
mod support;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mbrotli::compressor::parallel::{
    BatchConfig, ParallelCompressor, ParallelConfig, SegmentSize, TaskCount,
};
use mbrotli::{Compressor, EncoderConfig, Quality};
use std::hint::black_box;

fn encode(
    compressor: &mut ParallelCompressor,
    pool: &rayon::ThreadPool,
    input: &[u8],
    tasks: usize,
    out: &mut Vec<u8>,
) {
    let mut batch = compressor
        .prepare_slice(
            input,
            BatchConfig::memory(
                TaskCount::try_from(tasks).unwrap(),
                input.len() * 3 + (1 << 20),
            ),
        )
        .unwrap();
    if tasks == 1 {
        batch.run_inline().unwrap();
    } else {
        let jobs = batch.take_tasks().unwrap();
        pool.scope(|scope| {
            for job in jobs {
                scope.spawn(move |_| job.run());
            }
        });
    }
    out.clear();
    batch.finish_into(out).unwrap();
}
fn benchmarks(c: &mut Criterion) {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let text = std::fs::read("brotli-ffi/vendor/brotli/tests/testdata/alice29.txt").unwrap();
    let expand = |pattern: &[u8], size: usize| {
        pattern
            .iter()
            .copied()
            .cycle()
            .take(size)
            .collect::<Vec<_>>()
    };
    let mut seed = 1u64;
    let random: Vec<_> = (0..1 << 20)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    let corpora = [
        ("tiny", b"small input".to_vec()),
        ("text-1MiB", expand(&text, 1 << 20)),
        (
            "binary-1MiB",
            expand(&(0..=255).collect::<Vec<_>>(), 1 << 20),
        ),
        ("random-1MiB", random),
        ("zeros-1MiB", vec![0; 1 << 20]),
        ("text-16MiB", expand(&text, 16 << 20)),
    ];
    for q in [0, 1, 5, 9, 11] {
        let cfg = EncoderConfig::default().with_quality(Quality::try_from(q).unwrap());
        for (name, input) in &corpora {
            let segment = if input.len() >= 8 << 20 {
                SegmentSize::DEFAULT
            } else {
                SegmentSize::try_from(256 << 10).unwrap()
            };
            let parallel = ParallelConfig::from(segment)
                .with_minimum_parallel_size(0)
                .with_max_retained_workers(4);
            let mut compressor = ParallelCompressor::new(cfg, parallel).unwrap();
            let mut out = Vec::new();
            encode(&mut compressor, &pool, input, 1, &mut out);
            let expected = out.clone();
            encode(&mut compressor, &pool, input, 4, &mut out);
            assert_eq!(out, expected);
            assert_eq!(support::c_decompress(&out, input.len()).unwrap(), *input);
            let serial = support::c_compress(q.into(), 22, input);
            let rust = Compressor::new(cfg).unwrap().compress(input).unwrap();
            assert_eq!(rust, serial);
            assert_eq!(support::c_decompress(&serial, input.len()).unwrap(), *input);
            eprintln!(
                "q{q}/{name}: input={} parallel={} serial={} segment={}",
                input.len(),
                out.len(),
                serial.len(),
                segment.get()
            );
            let mut group = c.benchmark_group(format!("parallel/q{q}/{name}"));
            group.throughput(Throughput::Bytes(input.len() as u64));
            for tasks in [1, 4] {
                group.bench_with_input(BenchmarkId::new("tasks", tasks), &tasks, |b, &tasks| {
                    b.iter(|| {
                        encode(&mut compressor, &pool, black_box(input), tasks, &mut out);
                        black_box(&out);
                    })
                });
            }
            group.bench_function("c-serial-cold", |b| {
                b.iter(|| black_box(support::c_compress(q.into(), 22, black_box(input))))
            });
            group.bench_function("rust-serial-cold", |b| {
                b.iter(|| {
                    black_box(
                        Compressor::new(cfg)
                            .unwrap()
                            .compress(black_box(input))
                            .unwrap(),
                    )
                })
            });
            group.finish();
        }
    }
}
criterion_group!(benches, benchmarks);
criterion_main!(benches);
