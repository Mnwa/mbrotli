//! Byte-for-byte differential tests against the pinned C encoder.
//!
//! Quality 0 and quality 1 are ports of `google/brotli` v1.2.0 (commit
//! `028fb5a`), so with identical input, quality, window size and mode the two
//! encoders must emit identical bytes. Anything else is a porting bug.

mod support;

use mbrotli::Brotli;
use support::{
    IMPLEMENTED_QUALITIES, boundary_corpora, c_compress, params, prefix_for, quality_number,
    structural_corpora,
};

/// Compares one input against the C encoder for one parameter set.
fn assert_matches_c(name: &str, data: &[u8], lgwin: u8) {
    let compressor = Brotli::default().compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let data = prefix_for(quality, data);
        let expected = c_compress(quality_number(quality), lgwin as i32, data);
        let actual = compressor
            .compress(params(quality, lgwin), data)
            .expect("compression failed");
        assert_eq!(
            actual,
            expected,
            "case {name}, quality {}, lgwin {lgwin}: {} bytes vs {} bytes",
            usize::from(quality),
            actual.len(),
            expected.len()
        );
    }
}

#[test]
fn structural_corpora_match_the_c_encoder() {
    for corpus in structural_corpora() {
        assert_matches_c(&corpus.name, &corpus.data, 22);
    }
}

#[test]
fn boundary_lengths_match_the_c_encoder() {
    for corpus in boundary_corpora() {
        assert_matches_c(&corpus.name, &corpus.data, 22);
    }
}

#[test]
fn every_window_size_matches_the_c_encoder() {
    let corpora = structural_corpora();
    for lgwin in 10..=24u8 {
        for corpus in &corpora {
            if corpus.data.len() > 300_000 {
                continue;
            }
            assert_matches_c(&corpus.name, &corpus.data, lgwin);
        }
    }
}
