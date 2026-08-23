//! Engine-neutral fuzz target bodies.
//!
//! Each function here is one target: it takes the prepared [`Context`] and one
//! fuzz input, and panics when its oracle is violated. The binaries under
//! `src/bin/` wrap these in `afl::fuzz!`, and `tests/regressions.rs` replays
//! committed inputs through the very same functions, so a finding reproduces
//! identically under AFL, under `cargo test` and under a debugger.

use crate::{Context, FAST_QUALITIES, assert_round_trip, c_compress, cap, decode_case};
use mbrotli::Brotli;
use mbrotli::compressor::{
    BrotliCompressError, CompressParams, ParseQualityLevelError, ParseWindowBitsError,
    QualityLevel, WindowBits,
};
use std::ffi::c_int;
use std::io::{Read, Write};

/// Signature every target body shares: prepared state, then one fuzz input.
pub type TargetFn = fn(&Context, &[u8]);

/// Every target, addressable by the name of its binary.
///
/// `tests/regressions.rs` walks `regressions/<name>/` and replays each file
/// through the matching body, so adding a target here is all it takes to give
/// it a regression corpus.
pub const TARGETS: &[(&str, TargetFn)] = &[
    ("q0_roundtrip", q0_roundtrip),
    ("q1_roundtrip", q1_roundtrip),
    ("params_roundtrip", params_roundtrip),
    ("simd_equivalence", simd_equivalence),
    ("differential_c", differential_c),
    ("streaming_equivalence", streaming_equivalence),
    ("output_capacity", output_capacity),
    ("parameter_parsing", parameter_parsing),
];

/// Quality 0 must never panic and must always round-trip.
pub fn q0_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q0, data);
}

/// Quality 1 must never panic and must always round-trip.
pub fn q1_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q1, data);
}

fn fixed_quality_roundtrip(ctx: &Context, quality: QualityLevel, data: &[u8]) {
    let data = cap(data);
    let params = CompressParams::new(quality, WindowBits::DEFAULT);
    let bound = ctx
        .compressor
        .calculate_bound(&params, data.len())
        .expect("bound overflowed");
    let compressed = ctx
        .compressor
        .compress(params, data)
        .expect("compression failed");
    assert!(compressed.len() <= bound, "output exceeded the bound");
    assert_round_trip(data, &compressed);
}

/// Randomised legal settings must round-trip and stay deterministic.
pub fn params_roundtrip(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let bound = ctx
        .compressor
        .calculate_bound(&case.params, case.data.len())
        .expect("bound overflowed");
    let compressed = ctx
        .compressor
        .compress(case.params, case.data)
        .expect("compression failed");
    assert!(compressed.len() <= bound, "output exceeded the bound");
    let again = ctx
        .compressor
        .compress(case.params, case.data)
        .expect("compression failed");
    assert_eq!(compressed, again, "compression is not deterministic");
    assert_round_trip(case.data, &compressed);
}

/// Every SIMD backend must emit exactly the same bytes.
pub fn simd_equivalence(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let mut reference: Option<Vec<u8>> = None;
    for &level in &ctx.levels {
        let actual = Brotli::from(level)
            .compressor()
            .compress(case.params, case.data)
            .expect("compression failed");
        match &reference {
            None => reference = Some(actual),
            Some(expected) => assert_eq!(&actual, expected, "backends disagree"),
        }
    }
}

/// The encoder must stay byte identical to the pinned C reference.
pub fn differential_c(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let quality = usize::from(case.params.quality()) as c_int;
    let lgwin = usize::from(case.params.lgwin()) as c_int;
    let expected = c_compress(quality, lgwin, case.data);
    let actual = ctx
        .compressor
        .compress(case.params, case.data)
        .expect("compression failed");
    assert_eq!(actual, expected, "the Rust and C encoders disagree");
}

/// Chunk boundaries must not change the stream the adapters produce.
pub fn streaming_equivalence(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);

    let mut sink = ctx.compressor.compress_writer(case.params, Vec::new());
    for piece in case.data.chunks(case.chunk) {
        sink.write_all(piece).expect("write failed");
    }
    let written = sink.finish().expect("finish failed");

    let mut source = ctx.compressor.compress_reader(case.params, case.data);
    let mut read = Vec::new();
    let mut buffer = vec![0u8; case.chunk];
    loop {
        let count = source.read(&mut buffer).expect("read failed");
        if count == 0 {
            break;
        }
        read.extend_from_slice(&buffer[..count]);
    }

    assert_eq!(written, read, "the writer and reader adapters disagree");
    assert_round_trip(case.data, &written);
}

/// The slice entry point must respect an exact and a one-byte-short buffer.
pub fn output_capacity(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let expected = ctx
        .compressor
        .compress(case.params, case.data)
        .expect("compression failed");

    let mut exact = vec![0u8; expected.len()];
    let written = ctx
        .compressor
        .compress_to_slice(case.params, case.data, &mut exact)
        .expect("an exactly sized buffer must be accepted");
    assert_eq!(written, expected.len(), "written length differs");
    assert_eq!(exact, expected, "slice output differs");

    if expected.is_empty() {
        return;
    }
    let mut short = vec![0u8; expected.len() - 1];
    let outcome = ctx
        .compressor
        .compress_to_slice(case.params, case.data, &mut short);
    assert!(
        matches!(outcome, Err(BrotliCompressError::OutputTooSmall)),
        "a short buffer must be reported, not truncated"
    );
}

/// Parameter parsing must reject illegal settings and never panic.
///
/// The other targets only ever build legal parameters, which leaves the
/// validating conversions and the unimplemented-quality path unexercised. This
/// target drives both from raw bytes: byte 0 is a numeric quality, byte 1 a
/// numeric window size, and the rest is a payload. It asserts the documented
/// contract of [`QualityLevel::try_from`] and [`WindowBits::try_from`], and
/// that every entry point reports an unimplemented quality rather than
/// panicking or emitting a stream.
pub fn parameter_parsing(ctx: &Context, input: &[u8]) {
    let (header, data) = input.split_at(input.len().min(2));
    let data = cap(data);

    // Concentrated on the interesting neighbourhood: the 0..=11 range the API
    // models, the unrepresentable 10, and the first values above the ceiling.
    let quality_value = usize::from(header.first().copied().unwrap_or(0)) % 20;
    let window_value = usize::from(header.get(1).copied().unwrap_or(22));

    let quality = QualityLevel::try_from(quality_value);
    match (quality_value, &quality) {
        (10, Err(ParseQualityLevelError::Unrepresentable)) => {}
        (0..=11, Ok(parsed)) => assert_eq!(
            usize::from(*parsed),
            quality_value,
            "quality did not round-trip"
        ),
        (12.., Err(ParseQualityLevelError::UpperBound)) => {}
        _ => panic!("quality {quality_value} produced {quality:?}"),
    }

    let window = WindowBits::try_from(window_value);
    match (window_value, &window) {
        (..10, Err(ParseWindowBitsError::LowerBound)) => {}
        (10..=24, Ok(parsed)) => assert_eq!(
            usize::from(*parsed),
            window_value,
            "window bits did not round-trip"
        ),
        (25.., Err(ParseWindowBitsError::UpperBound)) => {}
        _ => panic!("window {window_value} produced {window:?}"),
    }

    let (Ok(quality), Ok(window)) = (quality, window) else {
        return;
    };
    let params = CompressParams::new(quality, window);

    if FAST_QUALITIES
        .iter()
        .any(|&implemented| usize::from(implemented) == quality_value)
    {
        let compressed = ctx
            .compressor
            .compress(params, data)
            .expect("an implemented quality must compress");
        assert_round_trip(data, &compressed);
        return;
    }

    // Every entry point has to refuse an unimplemented quality the same way,
    // and the streaming adapters have to carry that refusal out through
    // `std::io::Error` rather than panicking.
    assert!(
        matches!(
            ctx.compressor.compress(params, data),
            Err(BrotliCompressError::UnsupportedQuality(reported)) if reported == quality_value
        ),
        "quality {quality_value} must be reported as unimplemented"
    );

    let mut scratch = vec![0u8; data.len() + 64];
    assert!(
        matches!(
            ctx.compressor.compress_to_slice(params, data, &mut scratch),
            Err(BrotliCompressError::UnsupportedQuality(reported)) if reported == quality_value
        ),
        "the slice entry point must report quality {quality_value}"
    );

    let mut sink = ctx.compressor.compress_writer(params, Vec::new());
    let refused = sink.write_all(data).is_err() || sink.finish().is_err();
    assert!(refused, "the writer must refuse quality {quality_value}");

    let mut source = ctx.compressor.compress_reader(params, data);
    let mut drained = Vec::new();
    assert!(
        source.read_to_end(&mut drained).is_err(),
        "the reader must refuse quality {quality_value}"
    );
}
