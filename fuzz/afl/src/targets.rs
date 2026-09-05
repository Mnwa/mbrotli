//! Engine-neutral fuzz target bodies.
//!
//! Each function here is one target: it takes the prepared [`Context`] and one
//! fuzz input, and panics when its oracle is violated. The binaries under
//! `src/bin/` wrap these in `afl::fuzz!`, and `tests/regressions.rs` replays
//! committed inputs through the very same functions, so a finding reproduces
//! identically under AFL, under `cargo afl test` and under a debugger.

use crate::{
    Context, IMPLEMENTED_QUALITIES, assert_round_trip, c_compress_with, c_decompress_large_window,
    cap, decode_case,
};
use mbrotli::dictionary::{DictionaryBuilder, DictionaryError, DictionaryLimits};
use mbrotli::io::FinishError;
use mbrotli::{
    Compressor, ConfigError, EncodeError, EncoderConfig, EncoderStatus, Operation, Quality,
    StreamConfig, Window, WindowEncoding,
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
    ("dictionary", dictionary),
    ("compressor_lifecycle", compressor_lifecycle),
];

/// Quality 0 must never panic and must always round-trip.
pub fn q0_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q0, data);
}

/// Quality 1 must never panic and must always round-trip.
pub fn q1_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q1, data);
}

fn fixed_quality_roundtrip(ctx: &Context, quality: Quality, data: &[u8]) {
    let data = cap(data);
    let config = EncoderConfig::default().with_quality(quality);
    let bound = Compressor::max_compressed_size(data.len()).expect("bound overflowed");
    let compressed = ctx
        .encoder(config)
        .compress(data)
        .expect("compression failed");
    assert!(compressed.len() <= bound, "output exceeded the bound");
    assert_round_trip(data, &compressed);
}

/// Quality 3 must never panic and must always round-trip.
pub fn q3_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q3, data);
}

/// Quality 4 must never panic and must always round-trip.
pub fn q4_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q4, data);
}

/// Quality 5 must never panic and must always round-trip.
pub fn q5_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q5, data);
}

/// Quality 6 must never panic and must always round-trip.
pub fn q6_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q6, data);
}

/// Quality 7 must never panic and must always round-trip.
pub fn q7_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q7, data);
}

/// Quality 8 must never panic and must always round-trip.
pub fn q8_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q8, data);
}

/// Quality 9 must never panic and must always round-trip.
pub fn q9_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q9, data);
}

/// Quality 10 must never panic and must always round-trip.
pub fn q10_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q10, data);
}

/// Quality 11 must never panic and must always round-trip.
pub fn q11_roundtrip(ctx: &Context, data: &[u8]) {
    fixed_quality_roundtrip(ctx, Quality::Q11, data);
}

/// Randomised legal settings must round-trip and stay deterministic.
pub fn params_roundtrip(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let bound = Compressor::max_compressed_size(case.data.len()).expect("bound overflowed");
    let mut encoder = ctx.encoder(case.config);

    let compressed = encoder.compress(case.data).expect("compression failed");
    assert!(compressed.len() <= bound, "output exceeded the bound");

    // A reused compressor and a fresh one have to agree, which is the whole
    // contract the retained workspace lives under.
    let again = encoder.compress(case.data).expect("compression failed");
    assert_eq!(compressed, again, "a reused compressor changed the output");
    let fresh = ctx
        .encoder(case.config)
        .compress(case.data)
        .expect("compression failed");
    assert_eq!(compressed, fresh, "a warm compressor left a cold one");

    assert_round_trip(case.data, &compressed);
}

/// Every SIMD backend must emit exactly the same bytes.
pub fn simd_equivalence(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let mut reference: Option<Vec<u8>> = None;
    for &level in &ctx.levels {
        let actual = ctx
            .encoder_on(level, case.config)
            .compress(case.data)
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
    let expected = c_compress_with(&case.config, case.data);
    let actual = ctx
        .encoder(case.config)
        .compress(case.data)
        .expect("compression failed");
    assert_eq!(actual, expected, "the Rust and C encoders disagree");
}

/// Every streaming shape must reach the same stream as every other.
pub fn streaming_equivalence(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let mut encoder = ctx.encoder(case.config);

    let written = {
        let mut sink = encoder
            .writer(Vec::new(), case.stream)
            .expect("a legal stream");
        for piece in case.data.chunks(case.chunk.max(1)) {
            sink.write_all(piece).expect("write failed");
        }
        sink.finish()
            .map_err(FinishError::into_error)
            .expect("finish failed")
    };

    let read = {
        let mut source = encoder
            .reader(case.data, case.stream)
            .expect("a legal stream");
        let mut output = Vec::new();
        let mut buffer = vec![0u8; case.chunk.max(1)];
        loop {
            let count = source.read(&mut buffer).expect("read failed");
            if count == 0 {
                break output;
            }
            output.extend_from_slice(&buffer[..count]);
        }
    };

    let session = drive_session(&mut encoder, case.data, case.chunk.max(1), case.stream);

    assert_eq!(written, read, "the writer and reader adapters disagree");
    assert_eq!(written, session, "the writer and the session disagree");
    assert_round_trip(case.data, &written);

    // The stream declares the payload's true length, so it has to reach the
    // bytes the one-shot path produces — except where that path applies the
    // reference's own one-shot shortcuts.
    if !case.data.is_empty() {
        let one_shot = encoder.compress(case.data).expect("compression failed");
        if one_shot.len() <= written.len() {
            // A stream that grew is rewritten as uncompressed meta-blocks by the
            // one-shot path alone; anything else has to agree exactly.
            let fallback = one_shot.len() < written.len();
            assert!(
                fallback || one_shot == written,
                "the one-shot and streaming paths disagree"
            );
        }
    }
}

/// Drives `data` through a session in `chunk` sized steps.
fn drive_session(
    encoder: &mut Compressor,
    data: &[u8],
    chunk: usize,
    stream: StreamConfig,
) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = vec![0u8; chunk];
    let mut session = encoder.start(stream).expect("a legal stream");
    let mut offset = 0usize;
    loop {
        let take = (data.len() - offset).min(chunk);
        let operation = if offset + take == data.len() {
            Operation::Finish
        } else {
            Operation::Process
        };
        let progress = session
            .process(&data[offset..offset + take], &mut buffer, operation)
            .expect("the session failed");
        // A call that moved nothing has to say why, or a caller would spin.
        assert!(
            progress.consumed > 0
                || progress.produced > 0
                || matches!(
                    progress.status,
                    EncoderStatus::NeedsInput
                        | EncoderStatus::NeedsOutput
                        | EncoderStatus::Finished
                ),
            "the session reported no progress and no reason"
        );
        offset += progress.consumed;
        output.extend_from_slice(&buffer[..progress.produced]);
        if progress.status == EncoderStatus::Finished {
            assert!(session.is_finished());
            return output;
        }
    }
}

/// The slice entry point must respect an exact and a one-byte-short buffer.
pub fn output_capacity(ctx: &Context, input: &[u8]) {
    let case = decode_case(input);
    let mut encoder = ctx.encoder(case.config);
    let expected = encoder.compress(case.data).expect("compression failed");

    let mut exact = vec![0u8; expected.len()];
    let written = encoder
        .compress_to_slice(case.data, &mut exact)
        .expect("an exactly sized buffer must be accepted");
    assert_eq!(written, expected.len(), "written length differs");
    assert_eq!(exact, expected, "slice output differs");

    // Appending has to leave whatever the destination already held untouched.
    let mut appended = b"a prefix".to_vec();
    let range = encoder
        .compress_into(case.data, &mut appended)
        .expect("appending failed");
    assert_eq!(range.start, 8, "the prefix moved");
    assert_eq!(
        range.end,
        appended.len(),
        "the range does not reach the end"
    );
    assert_eq!(&appended[..8], b"a prefix", "the prefix changed");
    assert_eq!(
        &appended[range],
        expected.as_slice(),
        "appended bytes differ"
    );

    if expected.is_empty() {
        return;
    }
    let mut short = vec![0u8; expected.len() - 1];
    let outcome = encoder.compress_to_slice(case.data, &mut short);
    assert!(
        matches!(outcome, Err(EncodeError::OutputTooSmall { .. })),
        "a short buffer must be reported, not truncated"
    );
    // And the compressor is still usable afterwards.
    assert_eq!(
        encoder.compress(case.data).expect("compression failed"),
        expected,
        "a failed call changed the next one"
    );
}

/// Parameter parsing must reject illegal settings and never panic.
///
/// The other targets only ever build legal configurations, which leaves the
/// validating conversions and the refusal paths unexercised. This target drives
/// both from raw bytes: byte 0 is a numeric quality, byte 1 a numeric window
/// size, and the rest is a payload.
pub fn parameter_parsing(ctx: &Context, input: &[u8]) {
    let (header, data) = input.split_at(input.len().min(2));
    let data = cap(data);

    // Concentrated on the interesting neighbourhood: the 0..=11 range the API
    // models, and the first values above the ceiling.
    let quality_value = (header.first().copied().unwrap_or(0)) % 20;
    let window_value = header.get(1).copied().unwrap_or(22);

    let quality = Quality::try_from(quality_value);
    match (quality_value, &quality) {
        (0..=11, Ok(parsed)) => {
            assert_eq!(parsed.get(), quality_value, "quality did not round-trip")
        }
        (12.., Err(ConfigError::Quality { requested })) => {
            assert_eq!(*requested, quality_value);
        }
        _ => panic!("quality {quality_value} produced {quality:?}"),
    }

    let window = Window::standard(window_value);
    match (window_value, &window) {
        (..10 | 25.., Err(ConfigError::StandardWindow { requested })) => {
            assert_eq!(*requested, window_value);
        }
        (10..=24, Ok(parsed)) => {
            assert_eq!(
                parsed.bits(),
                window_value,
                "window bits did not round-trip"
            );
            assert_eq!(
                parsed.encoding(),
                WindowEncoding::Standard,
                "standard() must select the ordinary header"
            );
        }
        _ => panic!("window {window_value} produced {window:?}"),
    }

    let (Ok(quality), Ok(window)) = (quality, window) else {
        return;
    };
    let config = EncoderConfig::default()
        .with_quality(quality)
        .with_window(window);

    // Every quality the format defines now has an encoder, so a legal pair is
    // always accepted and always decodes back.
    assert!(
        IMPLEMENTED_QUALITIES
            .iter()
            .any(|implemented| implemented.get() == quality_value),
        "quality {quality_value} parsed but is not in the implemented set"
    );
    let compressed = ctx
        .encoder(config)
        .compress(data)
        .expect("an implemented quality must compress");
    assert_round_trip(data, &compressed);

    // The refusal that is left: qualities at or below two write distances
    // through a model built for the RFC 7932 alphabet, so they cannot carry a
    // large window. It is reported when the compressor is built, before any
    // input has been touched.
    let Ok(wide) = Window::large(window_value.clamp(10, 62)) else {
        return;
    };
    let wide = config.with_window(wide);
    let outcome = Compressor::new(wide);
    if quality_value <= 2 {
        assert!(
            matches!(
                outcome,
                Err(ConfigError::LargeWindowUnsupportedForQuality { quality: reported })
                    if reported == quality
            ),
            "quality {quality_value} must refuse a large window"
        );
    } else {
        assert!(
            outcome.is_ok(),
            "quality {quality_value} must accept a large window"
        );
    }
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
/// exceeds the bound, or a declaration wider than the C decoder's limit changes
/// anything but the six header bits.
pub fn large_window(ctx: &Context, input: &[u8]) {
    let (head, rest) = input.split_at(input.len().min(1));
    let case = decode_case(rest);
    // Concentrated on the legal range and the first values outside it.
    let requested = head.first().copied().unwrap_or(22) % 70;

    let window = match Window::large(requested) {
        Ok(window) => {
            assert_eq!(window.bits(), requested, "the window did not round-trip");
            assert_eq!(
                window.encoding(),
                WindowEncoding::Large,
                "large() must select the large header"
            );
            window
        }
        Err(ConfigError::LargeWindow {
            requested: reported,
        }) => {
            assert_eq!(reported, requested);
            assert!(
                !(10..=62).contains(&requested),
                "{requested} is a legal window"
            );
            return;
        }
        Err(other) => panic!("window {requested} produced {other:?}"),
    };

    let config = case.config.with_window(window);
    let quality = config.quality();

    // The qualities that may write distances through the format's fixed code
    // refuse when the compressor is built, rather than dropping the request.
    if quality <= Quality::Q2 {
        assert!(
            matches!(
                Compressor::new(config),
                Err(ConfigError::LargeWindowUnsupportedForQuality { .. })
            ),
            "a static-entropy quality must refuse a large window"
        );
        return;
    }

    let bound = Compressor::max_compressed_size(case.data.len()).expect("bound overflowed");
    let mut encoder = ctx.encoder(config);
    let compressed = encoder.compress(case.data).expect("compression failed");
    assert!(compressed.len() <= bound, "output exceeded the bound");
    let again = encoder.compress(case.data).expect("compression failed");
    assert_eq!(compressed, again, "compression is not deterministic");

    for &level in &ctx.levels {
        let actual = ctx
            .encoder_on(level, config)
            .compress(case.data)
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
    let narrow_window = Window::large(C_DECODER_MAX_WINDOW_BITS).expect("thirty is a legal window");
    let narrow = ctx
        .encoder(config.with_window(narrow_window))
        .compress(case.data)
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

/// Dictionaries one prepared dictionary may hold, as RFC 9841 fixes it.
const MAX_ATTACHMENTS: usize = 15;

/// A prepared dictionary must be sound however it is built and used.
///
/// The first byte says how many prefix dictionaries to cut the payload into —
/// sweeping past the fifteen the format allows, so the refusal is driven from
/// fuzz input too — and the second tightens the resource limits, so a
/// deliberately impossible budget is reached as often as a generous one. The
/// rest is [`decode_case`]: the quality, the window and the payload the
/// dictionaries are cut from and then matched against.
///
/// The oracles are that preparation is a transaction (a refusal yields no
/// dictionary at all), that the accessors agree with what was attached, that
/// the distance mapping round-trips, that a dictionary is refused rather than
/// ignored where no match finder can read one, and that a stream compressed
/// against one decodes back through the reference decoder with the same
/// dictionary attached.
///
/// # Panics
///
/// Panics when any of those is violated.
pub fn dictionary(ctx: &Context, input: &[u8]) {
    let (head, rest) = input.split_at(input.len().min(2));
    let case = decode_case(rest);
    let requested = usize::from(head.first().copied().unwrap_or(1)) % (MAX_ATTACHMENTS + 3);
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
        DictionaryLimits::default()
            .with_max_prefix_bytes(u64::from(squeeze))
            .with_max_retained_bytes(1 << 12)
    } else {
        DictionaryLimits::default()
    };

    let mut builder = DictionaryBuilder::new().with_limits(limits);
    for attachment in attachments.clone() {
        builder = builder.add_prefix(attachment);
    }

    let prepared = match builder.build() {
        Ok(prepared) => {
            assert!(
                requested <= MAX_ATTACHMENTS,
                "{requested} attachments should have been refused"
            );
            assert!(
                source_size > 0,
                "an empty dictionary should have been refused"
            );
            prepared
        }
        Err(DictionaryError::Empty) => {
            assert_eq!(source_size, 0, "a non-empty dictionary was called empty");
            return;
        }
        Err(DictionaryError::TooManyAttachments {
            attached: reported,
            limit,
        }) => {
            assert_eq!(limit, MAX_ATTACHMENTS);
            assert_eq!(reported, attached);
            assert!(attached > limit, "{attached} attachments are legal");
            return;
        }
        // A limit refusal is a transaction that produced nothing; there is no
        // partial dictionary to inspect.
        Err(DictionaryError::TooLarge { .. } | DictionaryError::PreparationTooLarge { .. }) => {
            return;
        }
        Err(other) => panic!("preparing a dictionary reported {other:?}"),
    };

    assert_eq!(prepared.attachment_count(), attached);
    assert_eq!(prepared.source_bytes(), source_size);
    assert!(prepared.retained_bytes() >= source_size);

    // A prefix offset and the backward distance addressing it are inverses.
    let total = source_size as u64;
    let max_backward = 1u64 << 20;
    assert_eq!(prepared.backward_distance(total, max_backward), None);
    assert_eq!(prepared.prefix_offset(max_backward, max_backward), None);
    assert_eq!(prepared.prefix_offset(u64::MAX, u64::MAX), None);
    for offset in [0u64, 1, total / 3, total / 2, total.saturating_sub(1)] {
        if offset >= total {
            continue;
        }
        let distance = prepared
            .backward_distance(offset, max_backward)
            .expect("inside the prefix");
        assert!(
            distance > max_backward,
            "a prefix distance must clear the window"
        );
        assert_eq!(
            prepared.prefix_offset(distance, max_backward),
            Some(offset),
            "the distance mapping did not round-trip"
        );
    }

    let quality = case.config.quality();
    let mut encoder = ctx.encoder(case.config);
    let outcome = encoder.compress_with_dictionary(&prepared, case.data);

    if quality < Quality::Q5 {
        assert!(
            matches!(
                outcome,
                Err(EncodeError::DictionaryUnsupportedForQuality { quality: reported })
                    if reported == quality
            ),
            "quality {} must refuse a dictionary it cannot read",
            quality.get()
        );
        // And refusing costs the caller nothing.
        assert!(
            !encoder
                .compress(case.data)
                .expect("compression failed")
                .is_empty()
        );
        return;
    }

    let compressed = outcome.expect("a dictionary quality must compress");
    let bound = Compressor::max_compressed_size(case.data.len()).expect("bound overflowed");
    assert!(compressed.len() <= bound, "output exceeded the bound");

    // Every dictionary entry point has to reach the same bytes.
    let mut buffer = vec![0u8; bound];
    let written = encoder
        .compress_with_dictionary_to_slice(&prepared, case.data, &mut buffer)
        .expect("the slice entry point failed");
    assert_eq!(
        &buffer[..written],
        compressed.as_slice(),
        "slice output differs"
    );

    let mut appended = Vec::new();
    let range = encoder
        .compress_with_dictionary_into(&prepared, case.data, &mut appended)
        .expect("the appending entry point failed");
    assert_eq!(
        &appended[range],
        compressed.as_slice(),
        "appended bytes differ"
    );

    // A dictionary never changes what an ordinary call produces.
    let plain = encoder.compress(case.data).expect("compression failed");
    let fresh = ctx
        .encoder(case.config)
        .compress(case.data)
        .expect("compression failed");
    assert_eq!(plain, fresh, "a dictionary call changed an ordinary one");
}

/// A compressor must survive any order of the things a caller can do to it.
///
/// The bytes drive a command sequence rather than one call, so reuse, failure,
/// reconfiguration, trimming and abandoned sessions are exercised in orders no
/// hand-written test would think to try. The oracle is the one that matters:
/// whatever has happened, a compressor still produces the bytes a fresh one
/// would for the same configuration.
pub fn compressor_lifecycle(ctx: &Context, input: &[u8]) {
    let (commands, rest) = input.split_at(input.len().min(8));
    let case = decode_case(rest);
    let mut encoder = ctx.encoder(case.config);
    let mut config = case.config;

    let expected = |ctx: &Context, config: EncoderConfig, data: &[u8]| {
        ctx.encoder(config)
            .compress(data)
            .expect("compression failed")
    };

    for &command in commands {
        match command % 8 {
            0 => {
                assert_eq!(
                    encoder.compress(case.data).expect("compression failed"),
                    expected(ctx, config, case.data),
                    "a reused compressor left a fresh one"
                );
            }
            1 => {
                let mut output = b"prefix".to_vec();
                let range = encoder
                    .compress_into(case.data, &mut output)
                    .expect("appending failed");
                assert_eq!(&output[..6], b"prefix", "the prefix changed");
                assert_eq!(
                    &output[range],
                    expected(ctx, config, case.data).as_slice(),
                    "appending left a fresh compressor"
                );
            }
            2 => {
                // A destination too small to hold anything must be reported and
                // must not poison what comes next.
                let mut cramped = [0u8; 1];
                let outcome = encoder.compress_to_slice(case.data, &mut cramped);
                assert!(
                    outcome.is_ok() || matches!(outcome, Err(EncodeError::OutputTooSmall { .. })),
                    "a short destination reported something else"
                );
            }
            3 => encoder.trim(mbrotli::RetentionPolicy::ReleaseAll),
            4 => {
                // A ceiling at or above what is already retained keeps it; the
                // reported figure has to be stable across the check itself.
                let retained = encoder.retained_bytes();
                encoder.trim(mbrotli::RetentionPolicy::Bounded {
                    max_bytes: retained,
                });
                assert_eq!(
                    encoder.retained_bytes(),
                    retained,
                    "a ceiling at the current size released something"
                );
            }
            5 => {
                // Reconfigure to another quality and back, which has to reset
                // every trace of the stream shape the old one had.
                let next = EncoderConfig::default()
                    .with_quality(Quality::try_from(command % 12).unwrap_or(Quality::Q1));
                if encoder.reconfigure(next).is_ok() {
                    config = next;
                }
            }
            6 => {
                // A session dropped before it finished abandons the stream.
                let mut buffer = [0u8; 64];
                let mut session = encoder.start(case.stream).expect("a legal stream");
                let _ = session.process(case.data, &mut buffer, Operation::Process);
            }
            _ => {
                let session = encoder.start(case.stream).expect("a legal stream");
                // Leaking a session is the one way to leave state behind, and
                // the compressor has to notice rather than trust it.
                std::mem::forget(session);
                assert!(matches!(
                    encoder.compress(case.data),
                    Err(EncodeError::AbandonedSession)
                ));
                encoder.recover();
            }
        }
    }

    assert_eq!(
        encoder.compress(case.data).expect("compression failed"),
        expected(ctx, config, case.data),
        "the compressor did not survive its lifecycle"
    );
}
