//! The C ABI against Google's one-shot C API, through identical calls.
//!
//! Both sides are called the way a C program calls them: one function per
//! payload, into a caller-owned buffer of `compress_bound` bytes, with codec
//! state built and released inside the call. Inputs are Google Brotli's own
//! test files plus a 64-byte prefix of one, which isolates per-call cost.
//!
//! Before timing, every stream is checked to be byte-identical between the two
//! encoders and to decode back to its payload through both decoders, and the
//! compressed sizes are printed, so a speed figure is always read against an
//! identical ratio.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use google_brotli_ffi as google;
use mbrotli_ffi::{MbrotliResult, mbrotli_compress, mbrotli_compress_bound, mbrotli_decompress};
use std::ffi::c_int;
use std::hint::black_box;
use std::path::Path;

/// Window every stream uses: the reference default.
const LGWIN: c_int = 22;

/// Qualities timed: both fast encoders, a greedy one and the densest.
const QUALITIES: [c_int; 4] = [1, 5, 9, 11];

/// Named inputs: text, binary, incompressible, and a tiny prefix.
fn corpora() -> Vec<(&'static str, Vec<u8>)> {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../brotli-ffi/vendor/brotli/tests/testdata");
    let read = |name: &str| {
        std::fs::read(directory.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    let alice = read("alice29.txt");
    vec![
        ("tiny-64B", alice[..64].to_vec()),
        ("alice29.txt", alice),
        ("mapsdatazrh", read("mapsdatazrh")),
        ("random_org_10k.bin", read("random_org_10k.bin")),
    ]
}

fn ours_compress(input: &[u8], output: &mut [u8], quality: c_int) -> usize {
    let mut len = output.len();
    // SAFETY: both buffers are live and have the stated lengths.
    let status = unsafe {
        mbrotli_compress(
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            &mut len,
            quality,
            LGWIN,
        )
    };
    assert_eq!(status, MbrotliResult::Ok);
    len
}

fn google_compress(input: &[u8], output: &mut [u8], quality: c_int) -> usize {
    let mut len = output.len();
    // SAFETY: both buffers are live and have the stated lengths.
    let accepted = unsafe {
        google::BrotliEncoderCompress(
            quality,
            LGWIN,
            google::BROTLI_MODE_GENERIC,
            input.len(),
            input.as_ptr(),
            &mut len,
            output.as_mut_ptr(),
        )
    };
    assert_eq!(accepted, google::BROTLI_TRUE);
    len
}

fn ours_decompress(input: &[u8], output: &mut [u8]) -> usize {
    let mut len = output.len();
    // SAFETY: both buffers are live and have the stated lengths.
    let status =
        unsafe { mbrotli_decompress(input.as_ptr(), input.len(), output.as_mut_ptr(), &mut len) };
    assert_eq!(status, MbrotliResult::Ok);
    len
}

fn google_decompress(input: &[u8], output: &mut [u8]) -> usize {
    let mut len = output.len();
    // SAFETY: both buffers are live and have the stated lengths.
    let result = unsafe {
        google::BrotliDecoderDecompress(input.len(), input.as_ptr(), &mut len, output.as_mut_ptr())
    };
    assert_eq!(result, google::BROTLI_DECODER_RESULT_SUCCESS);
    len
}

/// Compresses with both encoders, checks identity and both round trips, and
/// returns the stream.
fn validated_stream(input: &[u8], quality: c_int) -> Vec<u8> {
    let bound = mbrotli_compress_bound(input.len());
    let mut ours = vec![0; bound];
    let mut theirs = vec![0; bound];
    let ours_len = ours_compress(input, &mut ours, quality);
    ours.truncate(ours_len);
    let theirs_len = google_compress(input, &mut theirs, quality);
    theirs.truncate(theirs_len);
    assert_eq!(ours, theirs, "streams differ at q{quality}");
    let mut decoded = vec![0; input.len()];
    assert_eq!(ours_decompress(&ours, &mut decoded), input.len());
    assert_eq!(decoded, input);
    decoded.fill(0);
    assert_eq!(google_decompress(&ours, &mut decoded), input.len());
    assert_eq!(decoded, input);
    ours
}

fn compress(c: &mut Criterion) {
    let mut group = c.benchmark_group("c-abi-compress");
    for (name, input) in corpora() {
        group.throughput(Throughput::Bytes(input.len() as u64));
        let mut output = vec![0; mbrotli_compress_bound(input.len())];
        for quality in QUALITIES {
            let stream = validated_stream(&input, quality);
            println!(
                "{name} q{quality}: {} -> {} bytes",
                input.len(),
                stream.len()
            );
            let id = format!("{name}/q{quality}");
            group.bench_function(BenchmarkId::new("mbrotli", &id), |b| {
                b.iter(|| ours_compress(black_box(&input), black_box(&mut output), quality));
            });
            group.bench_function(BenchmarkId::new("google", &id), |b| {
                b.iter(|| google_compress(black_box(&input), black_box(&mut output), quality));
            });
        }
    }
    group.finish();
}

fn decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("c-abi-decompress");
    for (name, input) in corpora() {
        group.throughput(Throughput::Bytes(input.len() as u64));
        let mut output = vec![0; input.len()];
        for quality in QUALITIES {
            let stream = validated_stream(&input, quality);
            let id = format!("{name}/q{quality}");
            group.bench_function(BenchmarkId::new("mbrotli", &id), |b| {
                b.iter(|| ours_decompress(black_box(&stream), black_box(&mut output)));
            });
            group.bench_function(BenchmarkId::new("google", &id), |b| {
                b.iter(|| google_decompress(black_box(&stream), black_box(&mut output)));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, compress, decompress);
criterion_main!(benches);
