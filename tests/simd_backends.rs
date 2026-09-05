//! Every SIMD backend must produce the same bytes as the scalar fallback.
//!
//! The SIMD work is confined to the exact match-length scan, which only changes
//! how a length is discovered, never which length it is. Comparing decoded
//! output would not catch a divergence, so the streams are compared byte for
//! byte, including their bit length.

mod support;

use support::{
    IMPLEMENTED_QUALITIES, boundary_corpora, encoder_on, host_levels, prefix_for,
    structural_corpora,
};

/// Compresses one input on every backend and requires identical output.
fn assert_backends_agree(name: &str, data: &[u8], lgwin: u8) {
    let levels = host_levels();
    let (reference_name, reference_level) = levels[0];
    for quality in IMPLEMENTED_QUALITIES {
        let data = prefix_for(quality, data);
        let reference = encoder_on(reference_level, quality, lgwin)
            .compress(data)
            .expect("reference compression failed");
        for &(level_name, level) in &levels[1..] {
            let actual = encoder_on(level, quality, lgwin)
                .compress(data)
                .expect("compression failed");
            assert_eq!(
                actual.len(),
                reference.len(),
                "case {name}, quality {}, {level_name} vs {reference_name}: output length differs",
                quality.get()
            );
            assert_eq!(
                actual,
                reference,
                "case {name}, quality {}, {level_name} vs {reference_name}",
                quality.get()
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
    for lgwin in [10u8, 16, 18, 24] {
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
    for (index, (_, backend)) in levels.iter().enumerate() {
        assert!(
            levels[..index]
                .iter()
                .all(|(_, earlier)| earlier != backend)
        );
    }
    if mbrotli::Backend::default() != mbrotli::Backend::SCALAR {
        assert!(levels.len() >= 2, "the detected SIMD backend must also run");
    }
}
