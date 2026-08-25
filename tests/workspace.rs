//! A reused workspace must produce exactly what a fresh encoder produces.
//!
//! The workspace exists only to keep allocations alive; the moment it changes
//! a byte it is a bug, so every test here compares against the allocating
//! entry point over the same input.

mod support;

use mbrotli::Brotli;
use mbrotli::compressor::shared::SharedBrotliError;
use mbrotli::compressor::{
    BrotliCompressError, CompressParams, CompressWorkspace, QualityLevel, WindowBits,
};
use support::{IMPLEMENTED_QUALITIES, c_decompress, params, structural_corpora};

/// Window every case in this file uses.
const LGWIN: u8 = 22;

/// Payloads chosen to move the encoder between its internal thresholds.
fn payloads() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"a".to_vec(),
        b"hello hello hello hello hello".to_vec(),
        b"the quick brown fox jumps over the lazy dog. ".repeat(50),
        b"the quick brown fox jumps over the lazy dog. ".repeat(3000),
        (0u8..=255).cycle().take(70_000).collect(),
        vec![0u8; 100_000],
        (0u32..40_000)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect(),
    ]
}

#[test]
fn a_reused_workspace_matches_a_fresh_encoder() {
    let compressor = Brotli::default().compressor();
    for quality in IMPLEMENTED_QUALITIES {
        // The size hint is pinned so every payload resolves to the same
        // encoder shape: this is the case where reuse actually happens, and
        // therefore the case where a stale table would show up.
        let parameters = params(quality, LGWIN).with_size_hint(Some(1 << 20));
        let mut workspace = CompressWorkspace::default();
        for payload in payloads() {
            let expected = compressor
                .compress(parameters, &payload)
                .expect("fresh encoder failed");
            let reused = compressor
                .compress_with(&mut workspace, parameters, &payload)
                .expect("reused encoder failed");
            assert_eq!(
                reused,
                expected,
                "q{quality:?}: reuse changed the output for {} bytes",
                payload.len()
            );
        }
    }
}

#[test]
fn a_reused_workspace_matches_across_changing_parameters() {
    // Every call resolves to a different shape than the last, so the workspace
    // has to notice and rebuild rather than reset something incompatible.
    let compressor = Brotli::default().compressor();
    let payload = b"the quick brown fox jumps over the lazy dog. ".repeat(400);
    let mut workspace = CompressWorkspace::default();

    for quality in IMPLEMENTED_QUALITIES {
        for lgwin in [10u8, 16, 22, 24] {
            for hint in [None, Some(0), Some(1 << 21)] {
                let parameters = params(quality, lgwin).with_size_hint(hint);
                let expected = compressor
                    .compress(parameters, &payload)
                    .expect("fresh encoder failed");
                let reused = compressor
                    .compress_with(&mut workspace, parameters, &payload)
                    .expect("reused encoder failed");
                assert_eq!(
                    reused, expected,
                    "q{quality:?} lgwin {lgwin} hint {hint:?}: reuse changed the output"
                );
            }
        }
    }
}

#[test]
fn a_reused_workspace_matches_over_the_structural_corpora() {
    let compressor = Brotli::default().compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let parameters = params(quality, LGWIN);
        let mut workspace = CompressWorkspace::default();
        for corpus in structural_corpora() {
            let expected = compressor
                .compress(parameters, &corpus.data)
                .expect("fresh encoder failed");
            let reused = compressor
                .compress_with(&mut workspace, parameters, &corpus.data)
                .expect("reused encoder failed");
            assert_eq!(
                reused, expected,
                "q{quality:?}: reuse changed the output for {}",
                corpus.name
            );
        }
    }
}

#[test]
fn the_slice_entry_point_reuses_the_same_way() {
    let compressor = Brotli::default().compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let parameters = params(quality, LGWIN).with_size_hint(Some(1 << 20));
        let mut workspace = CompressWorkspace::default();
        for payload in payloads() {
            let expected = compressor
                .compress(parameters, &payload)
                .expect("fresh encoder failed");
            let mut buffer = vec![
                0u8;
                compressor
                    .calculate_bound(&parameters, payload.len())
                    .expect("bound")
            ];
            let written = compressor
                .compress_to_slice_with(&mut workspace, parameters, &payload, &mut buffer)
                .expect("reused encoder failed");
            assert_eq!(
                &buffer[..written],
                expected.as_slice(),
                "q{quality:?}: slice reuse changed the output for {} bytes",
                payload.len()
            );
        }
    }
}

#[test]
fn reused_output_still_round_trips() {
    let compressor = Brotli::default().compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let parameters = params(quality, LGWIN).with_size_hint(Some(1 << 20));
        let mut workspace = CompressWorkspace::default();
        for payload in payloads() {
            let compressed = compressor
                .compress_with(&mut workspace, parameters, &payload)
                .expect("reused encoder failed");
            let decoded = c_decompress(&compressed, payload.len().max(1))
                .expect("the decoder rejected a reused stream");
            assert_eq!(decoded, payload, "q{quality:?}: reuse broke the round trip");
        }
    }
}

#[test]
fn a_failed_call_does_not_poison_the_workspace() {
    // A short destination abandons the stream part-written. The next call has
    // to behave as if that had never happened.
    let compressor = Brotli::default().compressor();
    let payload = b"the quick brown fox jumps over the lazy dog. ".repeat(400);
    for quality in IMPLEMENTED_QUALITIES {
        let parameters = params(quality, LGWIN);
        let mut workspace = CompressWorkspace::default();

        let mut cramped = [0u8; 4];
        let outcome =
            compressor.compress_to_slice_with(&mut workspace, parameters, &payload, &mut cramped);
        assert!(
            matches!(outcome, Err(BrotliCompressError::OutputTooSmall)),
            "q{quality:?}: a four-byte buffer was accepted"
        );

        let after = compressor
            .compress_with(&mut workspace, parameters, &payload)
            .expect("the workspace did not recover");
        assert_eq!(
            after,
            compressor.compress(parameters, &payload).expect("fresh"),
            "q{quality:?}: a failed call changed the next one"
        );
    }
}

#[test]
fn a_refused_parameter_set_is_reported_through_the_workspace() {
    // The workspace must not swallow or defer a refusal: qualities at or below
    // two cannot carry a large window, and both entry points have to say so
    // whether or not an encoder is already retained.
    let compressor = Brotli::default().compressor();
    let mut workspace = CompressWorkspace::default();
    let mut buffer = [0u8; 256];

    // Retain an encoder first, so the refusal has to survive a warm cache.
    compressor
        .compress_with(&mut workspace, params(QualityLevel::Q5, LGWIN), b"payload")
        .expect("a legal call must compress");

    for quality in [QualityLevel::Q0, QualityLevel::Q1, QualityLevel::Q2] {
        let wide = CompressParams::new(quality, WindowBits::large(30).expect("legal"));
        let expected = usize::from(quality);
        assert!(
            matches!(
                compressor.compress_with(&mut workspace, wide, b"payload"),
                Err(BrotliCompressError::Shared(
                    SharedBrotliError::UnsupportedLargeWindow { quality: reported }
                )) if reported == expected
            ),
            "q{quality:?}: the vector entry point accepted a large window"
        );
        assert!(
            matches!(
                compressor.compress_to_slice_with(&mut workspace, wide, b"payload", &mut buffer),
                Err(BrotliCompressError::Shared(
                    SharedBrotliError::UnsupportedLargeWindow { quality: reported }
                )) if reported == expected
            ),
            "q{quality:?}: the slice entry point accepted a large window"
        );
    }

    // The workspace still works afterwards.
    assert_eq!(
        compressor
            .compress_with(&mut workspace, params(QualityLevel::Q5, LGWIN), b"payload")
            .expect("the workspace did not recover"),
        compressor
            .compress(params(QualityLevel::Q5, LGWIN), b"payload")
            .expect("fresh")
    );
}
