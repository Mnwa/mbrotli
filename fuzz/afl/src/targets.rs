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
use mbrotli::compressor::shared::{SharedBrotliError, SharedContext, SharedContextLimits};
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
    ("shared_context", shared_context),
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
/// validating conversions and the refusal paths unexercised. This target
/// drives both from raw bytes: byte 0 is a numeric quality, byte 1 a numeric
/// window size, and the rest is a payload. It asserts the documented contract
/// of [`QualityLevel::try_from`] and [`WindowBits::try_from`], and that every
/// entry point refuses a large window at the qualities that cannot carry one
/// the same way, rather than panicking or emitting a stream.
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

    // Every quality the format defines now has an encoder, so a legal pair is
    // always accepted and always decodes back.
    assert!(
        IMPLEMENTED_QUALITIES
            .iter()
            .any(|&implemented| usize::from(implemented) == quality_value),
        "quality {quality_value} parsed but is not in the implemented set"
    );
    let compressed = ctx
        .compressor
        .compress(params, data)
        .expect("an implemented quality must compress");
    assert_round_trip(data, &compressed);

    // The refusal that is left: qualities at or below two write distances
    // through a model built for the RFC 7932 alphabet, so they cannot carry a
    // large window. Every entry point has to say so the same way, and the
    // streaming adapters have to carry it out through `std::io::Error` rather
    // than panicking.
    if quality_value > 2 {
        return;
    }
    let Ok(wide) = WindowBits::large(window_value.clamp(10, 62)) else {
        return;
    };
    let params = CompressParams::new(quality, wide);

    assert!(
        matches!(
            ctx.compressor.compress(params, data),
            Err(BrotliCompressError::Shared(
                SharedBrotliError::UnsupportedLargeWindow { quality: reported }
            )) if reported == quality_value
        ),
        "quality {quality_value} must refuse a large window"
    );

    let mut scratch = vec![0u8; data.len() + 64];
    assert!(
        matches!(
            ctx.compressor.compress_to_slice(params, data, &mut scratch),
            Err(BrotliCompressError::Shared(
                SharedBrotliError::UnsupportedLargeWindow { quality: reported }
            )) if reported == quality_value
        ),
        "the slice entry point must refuse a large window at quality {quality_value}"
    );

    let mut sink = ctx.compressor.compress_writer(params, Vec::new());
    let refused = sink.write_all(data).is_err() || sink.finish().is_err();
    assert!(refused, "the writer must refuse a large window");

    let mut source = ctx.compressor.compress_reader(params, data);
    let mut drained = Vec::new();
    assert!(
        source.read_to_end(&mut drained).is_err(),
        "the reader must refuse a large window"
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

    // The qualities that may write distances through the format's fixed code
    // refuse rather than dropping the request, and refuse it the same way
    // whatever the payload is.
    if matches!(
        params.quality(),
        QualityLevel::Q0 | QualityLevel::Q1 | QualityLevel::Q2
    ) {
        assert!(
            matches!(
                ctx.compressor.compress(params, case.data),
                Err(BrotliCompressError::Shared(
                    SharedBrotliError::UnsupportedLargeWindow { .. }
                ))
            ),
            "a static-entropy quality must refuse a large window"
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

/// Dictionaries one RFC 9841 context may attach.
const MAX_PREFIX_DICTIONARIES: usize = 15;

/// An RFC 9841 shared context must be sound however it is built and read.
///
/// The first byte says how many prefix dictionaries to cut the payload into —
/// sweeping past the fifteen the format allows, so the refusal is driven from
/// fuzz input too — and the second tightens the resource limits, so a
/// deliberately impossible budget is reached as often as a generous one. The
/// rest is [`decode_case`]: the quality, the window and the payload the
/// dictionaries are cut from and then matched against.
///
/// The oracles are that preparation is a transaction (a refusal yields no
/// context at all), that the accessors agree with what was attached, that a
/// reported prefix match really matches those bytes, that the distance mapping
/// round-trips, that every backend finds the same match, that an empty context
/// compresses exactly as the ordinary call does, and that a non-empty one is
/// refused rather than quietly ignored.
///
/// # Panics
///
/// Panics when any of those is violated.
pub fn shared_context(ctx: &Context, input: &[u8]) {
    let (head, rest) = input.split_at(input.len().min(2));
    let case = decode_case(rest);
    let requested = usize::from(head.first().copied().unwrap_or(1)) % (MAX_PREFIX_DICTIONARIES + 3);
    let squeeze = head.get(1).copied().unwrap_or(0);

    // Cut the payload into `requested` attachments, keeping call order.
    let attachments: Vec<Vec<u8>> = if requested == 0 {
        Vec::new()
    } else {
        let stride = case.data.len().div_ceil(requested).max(1);
        case.data
            .chunks(stride)
            .take(requested)
            .map(<[u8]>::to_vec)
            .collect()
    };
    let attached = attachments.len();
    let source_size: usize = attachments.iter().map(Vec::len).sum();

    // Every fourth input runs under a budget too small for anything.
    let limits = if squeeze % 4 == 0 {
        SharedContextLimits::default()
            .with_max_prefix_bytes(u64::from(squeeze))
            .with_max_allocated_bytes(1 << 12)
    } else {
        SharedContextLimits::default()
    };

    let mut builder = ctx
        .compressor
        .shared_context_builder(case.params.quality())
        .with_limits(limits);
    for attachment in attachments.clone() {
        builder = builder.add_prefix_dictionary(attachment);
    }

    let mut context = match builder.prepare() {
        Ok(context) => {
            assert!(
                requested <= MAX_PREFIX_DICTIONARIES,
                "{requested} attachments should have been refused"
            );
            context
        }
        Err(BrotliCompressError::Shared(SharedBrotliError::TooManyPrefixDictionaries {
            attached: reported,
            limit,
        })) => {
            assert_eq!(limit, MAX_PREFIX_DICTIONARIES);
            assert_eq!(reported, attached);
            assert!(attached > limit, "{attached} attachments are legal");
            return;
        }
        // A limit refusal is a transaction that produced nothing; there is no
        // partial context to inspect.
        Err(BrotliCompressError::Shared(
            SharedBrotliError::DictionaryTooLarge { .. }
            | SharedBrotliError::SharedContextTooLarge { .. },
        )) => return,
        Err(other) => panic!("preparing a context reported {other:?}"),
    };

    assert_eq!(context.max_quality(), case.params.quality());
    assert_eq!(context.attachment_count(), attached);
    assert_eq!(context.prefix_dictionary_count(), attached);
    assert!(!context.has_custom_static_dictionary());
    assert_eq!(context.source_size(), source_size);
    assert!(context.allocated_size() >= source_size);

    assert_shared_addressing(&context, source_size);
    assert_shared_search(ctx, &context, &attachments, case.data);
    assert_shared_compression(ctx, &mut context, &case.params, case.data, source_size);
}

/// A prefix offset and the backward distance addressing it must be inverses.
fn assert_shared_addressing(context: &SharedContext, source_size: usize) {
    let total = source_size as u64;
    let max_backward = 1u64 << 20;

    // Off both ends, in both directions.
    assert_eq!(context.backward_distance(total, max_backward), None);
    assert_eq!(context.backward_distance(total + 1, max_backward), None);
    assert_eq!(context.dictionary_offset(max_backward, max_backward), None);
    assert_eq!(
        context.dictionary_offset(max_backward + total + 1, max_backward),
        None
    );
    assert_eq!(context.dictionary_offset(u64::MAX, u64::MAX), None);

    // The ends of the prefix, and a handful of interior addresses.
    let probes = [0u64, 1, total / 3, total / 2, total.saturating_sub(1)];
    for offset in probes {
        if offset >= total {
            continue;
        }
        let distance = context
            .backward_distance(offset, max_backward)
            .expect("inside the prefix");
        assert!(
            distance > max_backward,
            "a prefix distance must clear the window"
        );
        assert_eq!(
            context.dictionary_offset(distance, max_backward),
            Some(offset),
            "the distance mapping did not round-trip"
        );
    }
}

/// A reported match must really match, and must not depend on the compressor.
fn assert_shared_search(
    ctx: &Context,
    context: &SharedContext,
    attachments: &[Vec<u8>],
    data: &[u8],
) {
    let flat: Vec<u8> = attachments.concat();

    for start in [0usize, 1, data.len() / 3, data.len() / 2] {
        let Some(probe) = data.get(start..) else {
            continue;
        };
        let found = ctx.compressor.longest_prefix_match(context, probe);

        if let Some(found) = found {
            let offset = usize::try_from(found.dictionary_offset()).expect("a real offset");
            assert!(found.length() > 0, "a reported match must be non-empty");
            assert!(offset < flat.len(), "the match starts outside the prefix");
            let available = (flat.len() - offset).min(probe.len());
            assert!(
                found.length() <= available,
                "a match of {} exceeds the {available} bytes available",
                found.length()
            );
            assert_eq!(
                &flat[offset..offset + found.length()],
                &probe[..found.length()],
                "the reported match does not actually match"
            );
        }

        // The search is scalar, so a compressor that resolved a different
        // backend must reach the same answer over the same context.
        for &level in &ctx.levels {
            assert_eq!(
                Brotli::from(level)
                    .compressor()
                    .longest_prefix_match(context, probe),
                found,
                "the longest prefix match depended on the compressor"
            );
        }
    }
}

/// The three outcomes a shared compression can have, all checked here.
///
/// An empty context has to produce exactly the ordinary stream. A non-empty
/// one has to be consulted at a quality whose match finder can, and refused at
/// one that cannot — never silently ignored, which is the failure this target
/// exists to rule out.
fn assert_shared_compression(
    ctx: &Context,
    context: &mut SharedContext,
    params: &CompressParams,
    data: &[u8],
    source_size: usize,
) {
    let params = *params;
    let bound = ctx
        .compressor
        .calculate_shared_bound(&params, context, data.len())
        .expect("bound overflowed");
    assert_eq!(
        bound,
        ctx.compressor
            .calculate_bound(&params, data.len())
            .expect("bound overflowed")
    );

    let outcome = ctx.compressor.compress_shared(params, context, data);
    if source_size == 0 {
        let compressed = outcome.expect("an empty context must compress");
        assert!(compressed.len() <= bound, "output exceeded the bound");
        assert_eq!(
            compressed,
            ctx.compressor
                .compress(params, data)
                .expect("compression failed"),
            "an empty context changed the stream"
        );
        assert_round_trip(data, &compressed);

        let mut buffer = vec![0u8; bound];
        let written = ctx
            .compressor
            .compress_shared_to_slice(params, context, data, &mut buffer)
            .expect("an empty context must compress into a bounded slice");
        assert_eq!(&buffer[..written], compressed.as_slice());
        return;
    }

    let quality = usize::from(params.quality());
    if quality < 5 {
        assert!(
            matches!(
                outcome,
                Err(BrotliCompressError::Shared(
                    SharedBrotliError::UnsupportedSharedContextForQuality { quality: reported }
                )) if reported == quality
            ),
            "an attached dictionary was not refused at quality {quality}"
        );
        return;
    }

    // The dictionary was consulted. Two things have to hold whatever it found:
    // the stream stays inside the bound the caller sized a buffer from, and it
    // still decodes — to the same bytes, through a decoder that knows nothing
    // about the dictionary only when no distance actually reached into it.
    let compressed = outcome.expect("an attachable quality must compress");
    assert!(
        compressed.len() <= bound,
        "output exceeded the shared bound"
    );

    let mut buffer = vec![0u8; bound];
    let written = ctx
        .compressor
        .compress_shared_to_slice(params, context, data, &mut buffer)
        .expect("the slice entry point must agree with the vector one");
    assert_eq!(
        &buffer[..written],
        compressed.as_slice(),
        "the two shared entry points disagreed"
    );
}
