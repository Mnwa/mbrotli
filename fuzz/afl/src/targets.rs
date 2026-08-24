//! Engine-neutral fuzz target bodies.
//!
//! Each function here is one target: it takes the prepared [`Context`] and one
//! fuzz input, and panics when its oracle is violated. The binaries under
//! `src/bin/` wrap these in `afl::fuzz!`, and `tests/regressions.rs` replays
//! committed inputs through the very same functions, so a finding reproduces
//! identically under AFL, under `cargo test` and under a debugger.

use crate::{
    Context, IMPLEMENTED_QUALITIES, assert_round_trip, c_compress_with, c_decompress_large_window,
    cap, decode_case,
};
use mbrotli::Brotli;
use mbrotli::compressor::shared::SharedBrotliError;
use mbrotli::compressor::{
    BrotliCompressError, CompressParams, ParseQualityLevelError, ParseWindowBitsError,
    QualityLevel, WindowBits,
};
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
    ("q3_roundtrip", q3_roundtrip),
    ("q4_roundtrip", q4_roundtrip),
    ("q5_roundtrip", q5_roundtrip),
    ("q6_roundtrip", q6_roundtrip),
    ("q7_roundtrip", q7_roundtrip),
    ("q8_roundtrip", q8_roundtrip),
    ("q9_roundtrip", q9_roundtrip),
    ("q10_roundtrip", q10_roundtrip),
    ("q11_roundtrip", q11_roundtrip),
    ("params_roundtrip", params_roundtrip),
    ("simd_equivalence", simd_equivalence),
    ("differential_c", differential_c),
    ("streaming_equivalence", streaming_equivalence),
    ("output_capacity", output_capacity),
    ("parameter_parsing", parameter_parsing),
    ("large_window", large_window),
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

/// Quality 3 must never panic and must always round-trip.
pub fn q3_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q3, data);
}

/// Quality 4 must never panic and must always round-trip.
pub fn q4_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q4, data);
}

/// Quality 5 must never panic and must always round-trip.
pub fn q5_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q5, data);
}

/// Quality 6 must never panic and must always round-trip.
pub fn q6_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q6, data);
}

/// Quality 7 must never panic and must always round-trip.
pub fn q7_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q7, data);
}

/// Quality 8 must never panic and must always round-trip.
pub fn q8_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q8, data);
}

/// Quality 9 must never panic and must always round-trip.
pub fn q9_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q9, data);
}

/// Quality 10 must never panic and must always round-trip.
pub fn q10_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q10, data);
}

/// Quality 11 must never panic and must always round-trip.
pub fn q11_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, QualityLevel::Q11, data);
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
    // The empty input takes the one-shot shortcut in this crate and in the C
    // one-shot API, but not in the C streaming API this oracle uses.
    if case.data.is_empty() {
        return;
    }
    let expected = c_compress_with(&case.params, case.data);
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
    // models, and the first values above the ceiling.
    let quality_value = usize::from(header.first().copied().unwrap_or(0)) % 20;
    let window_value = header.get(1).copied().unwrap_or(22);

    let quality = QualityLevel::try_from(quality_value);
    match (quality_value, &quality) {
        (0..=11, Ok(parsed)) => assert_eq!(
            usize::from(*parsed),
            quality_value,
            "quality did not round-trip"
        ),
        (12.., Err(ParseQualityLevelError::UpperBound)) => {}
        _ => panic!("quality {quality_value} produced {quality:?}"),
    }

    let window = WindowBits::standard(window_value);
    match (window_value, &window) {
        (..10, Err(ParseWindowBitsError::LowerBound)) => {}
        (10..=24, Ok(parsed)) => {
            assert_eq!(
                parsed.bits(),
                window_value,
                "window bits did not round-trip"
            );
            assert!(
                !parsed.is_large(),
                "standard() must select the ordinary header"
            );
        }
        (25.., Err(ParseWindowBitsError::UpperBound)) => {}
        _ => panic!("window {window_value} produced {window:?}"),
    }

    let (Ok(quality), Ok(window)) = (quality, window) else {
        return;
    };
    let params = CompressParams::new(quality, window);

    if IMPLEMENTED_QUALITIES
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

/// Returns `params` with a different window and everything else untouched.
///
/// The window is part of `CompressParams`'s constructor rather than a setter,
/// so swapping it means rebuilding the value around it.
fn rebuild(params: &CompressParams, window: WindowBits) -> CompressParams {
    CompressParams::new(params.quality(), window)
        .with_mode(params.mode())
        .with_size_hint(params.size_hint())
        .with_block_bits(params.lgblock())
        .with_distance_codes(params.distance_codes())
        .with_literal_context_modeling(params.literal_context_modeling())
}

/// Widest window the pinned C decoder accepts (`BROTLI_LARGE_MAX_WBITS`).
///
/// RFC 9841 allows 62; the pinned reference is built for 32-bit arithmetic and
/// refuses to decode a wider declaration. Above this the target checks the
/// header and the payload against the stream the decoder did accept.
const C_DECODER_MAX_WINDOW_BITS: u8 = 30;

/// RFC 9841 large-window streams must round-trip and stay deterministic.
///
/// Reuses [`decode_case`] for the payload, the quality and the distance-code
/// layout, then overrides the window from a byte of its own. That byte sweeps
/// the whole `10..=62` range plus the values on either side of it, so the
/// validating conversion, the fourteen-bit header, the widened distance
/// alphabet and its per-meta-block retune are all driven from fuzz input.
///
/// # Panics
///
/// Panics when a legal window is refused, an illegal one is accepted, the
/// stream does not round-trip, the encoder is not deterministic, the output
/// exceeds the bound, or a declaration wider than the C decoder's limit
/// changes anything but the six header bits.
pub fn large_window(ctx: &Context, input: &[u8]) {
    let (head, rest) = input.split_at(input.len().min(1));
    let case = decode_case(rest);
    // Concentrated on the legal range and the first values outside it.
    let requested = head.first().copied().unwrap_or(22) % 70;

    let window = match WindowBits::large(requested) {
        Ok(window) => {
            assert_eq!(window.bits(), requested, "the window did not round-trip");
            assert!(window.is_large(), "large() must select the large header");
            window
        }
        Err(ParseWindowBitsError::LowerBound) => {
            assert!(requested < 10, "{requested} is a legal window");
            return;
        }
        Err(ParseWindowBitsError::LargeUpperBound) => {
            assert!(requested > 62, "{requested} is a legal window");
            return;
        }
        Err(other) => panic!("window {requested} produced {other:?}"),
    };

    let params = rebuild(&case.params, window);
    assert_eq!(params.lgwin(), window);
    assert!(params.lgwin().is_large());

    // Qualities zero and one refuse rather than dropping the request, and
    // refuse it the same way whatever the payload is.
    if matches!(params.quality(), QualityLevel::Q0 | QualityLevel::Q1) {
        assert!(
            matches!(
                ctx.compressor.compress(params, case.data),
                Err(BrotliCompressError::Shared(
                    SharedBrotliError::UnsupportedLargeWindow { .. }
                ))
            ),
            "a fast quality must refuse a large window"
        );
        return;
    }

    let bound = ctx
        .compressor
        .calculate_bound(&params, case.data.len())
        .expect("bound overflowed");
    let compressed = ctx
        .compressor
        .compress(params, case.data)
        .expect("compression failed");
    assert!(compressed.len() <= bound, "output exceeded the bound");
    let again = ctx
        .compressor
        .compress(params, case.data)
        .expect("compression failed");
    assert_eq!(compressed, again, "compression is not deterministic");

    for &level in &ctx.levels {
        let actual = Brotli::from(level)
            .compressor()
            .compress(params, case.data)
            .expect("compression failed");
        assert_eq!(actual, compressed, "backends disagree");
    }

    if case.data.is_empty() {
        // The one-shot shortcut answers an empty input with an ordinary
        // one-byte stream, exactly as the reference does.
        assert_eq!(compressed, vec![6], "an empty input must stay one byte");
        return;
    }

    if requested <= C_DECODER_MAX_WINDOW_BITS {
        let decoded = c_decompress_large_window(&compressed, case.data.len())
            .unwrap_or_else(|| panic!("the decoder rejected a {} byte stream", compressed.len()));
        assert_eq!(decoded, case.data, "decoded content differs");
        return;
    }

    // Above the decoder's limit, the encoder still keeps at most thirty bits of
    // history, so the stream must be the thirty-bit stream with a different
    // window written into its header — and that one has to decode.
    let narrow_window =
        WindowBits::large(C_DECODER_MAX_WINDOW_BITS).expect("thirty is a legal window");
    let narrow = ctx
        .compressor
        .compress(rebuild(&params, narrow_window), case.data)
        .expect("compression failed");
    let decoded = c_decompress_large_window(&narrow, case.data.len())
        .unwrap_or_else(|| panic!("the decoder rejected a {} byte stream", narrow.len()));
    assert_eq!(decoded, case.data, "decoded content differs");

    let mut expected = narrow;
    expected[1] = (expected[1] & 0xC0) | (requested & 0x3F);
    assert_eq!(
        compressed, expected,
        "a wider declaration changed more than the header"
    );
}
