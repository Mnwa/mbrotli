//! Every SIMD backend must produce the same bytes as the scalar fallback.
//!
//! The SIMD work is confined to the exact match-length scan, which only changes
//! how a length is discovered, never which length it is. Comparing decoded
//! output would not catch a divergence, so the streams are compared byte for
//! byte, including their bit length.

mod support;

use mbrotli::Brotli;
use support::{IMPLEMENTED_QUALITIES, boundary_corpora, host_levels, params, structural_corpora};

/// Compresses one input on every backend and requires identical output.
fn assert_backends_agree(name: &str, data: &[u8], lgwin: usize) {
    let levels = host_levels();
    let (reference_name, reference_level) = levels[0];
    for quality in IMPLEMENTED_QUALITIES {
        let reference = Brotli::from(reference_level)
            .compressor()
            .compress(params(quality, lgwin), data)
            .expect("reference compression failed");
        for &(level_name, level) in &levels[1..] {
            let actual = Brotli::from(level)
                .compressor()
                .compress(params(quality, lgwin), data)
                .expect("compression failed");
            assert_eq!(
                actual.len(),
                reference.len(),
                "case {name}, quality {}, {level_name} vs {reference_name}: output length differs",
                usize::from(quality)
            );
            assert_eq!(
                actual,
                reference,
                "case {name}, quality {}, {level_name} vs {reference_name}",
                usize::from(quality)
            );
        }
    }
}

#[test]
fn every_backend_agrees_on_structural_corpora() {
    for corpus in structural_corpora() {
        assert_backends_agree(&corpus.name, &corpus.data, 22);
    }
}

#[test]
fn every_backend_agrees_on_boundary_lengths() {
    for corpus in boundary_corpora() {
        assert_backends_agree(&corpus.name, &corpus.data, 22);
    }
}

#[test]
fn every_backend_agrees_across_window_sizes() {
    let corpora = structural_corpora();
    for lgwin in [10usize, 16, 18, 24] {
        for corpus in &corpora {
            assert_backends_agree(&corpus.name, &corpus.data, lgwin);
        }
    }
}

#[test]
fn the_scalar_fallback_is_actually_exercised() {
    let levels = host_levels();
    assert!(
        levels.iter().any(|&(name, _)| name == "fallback"),
        "the scalar fallback backend must be part of the matrix"
    );
    assert!(
        levels.len() >= 3,
        "expected more than one backend: {levels:?}"
    );
}
