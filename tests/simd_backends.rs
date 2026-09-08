//! Every available SIMD backend must produce the same bytes.
//! Scalar equivalence is covered by private unit tests.
//!
//! Match scans and high-quality histogram assignment preserve exact decisions,
//! including floating-point costs and histogram ties. Comparing decoded output
//! would not catch a divergence, so streams are compared byte for byte,
//! including their bit length.

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
fn the_public_backend_matrix_contains_only_required_backends() {
    let levels = host_levels();
    assert!(
        levels
            .iter()
            .any(|&(_, backend)| backend == mbrotli::Backend::default())
    );
    for (index, (_, backend)) in levels.iter().enumerate() {
        assert!(
            levels[..index]
                .iter()
                .all(|(_, earlier)| earlier != backend)
        );
    }
    if mbrotli::Backend::default().name() != "fallback" {
        assert!(levels.iter().all(|&(name, _)| name != "fallback"));
    }
}
