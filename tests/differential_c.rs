//! Byte-for-byte differential tests against the pinned C encoder.
//!
//! Every quality is a port of `google/brotli` v1.2.0 (commit `028fb5a`), so
//! with identical input, quality, window size and mode the two encoders must
//! emit identical bytes. Anything else is a porting bug.

mod support;

use support::{
    IMPLEMENTED_QUALITIES, boundary_corpora, c_compress, config, encoder, prefix_for,
    quality_number, structural_corpora,
};

/// Compares one input against the C encoder for one parameter set.
fn assert_matches_c(name: &str, data: &[u8], lgwin: u8) {
    for quality in IMPLEMENTED_QUALITIES {
        let data = prefix_for(quality, data);
        let expected = c_compress(quality_number(quality), i32::from(lgwin), data);
        let actual = encoder(quality, lgwin)
            .compress(data)
            .expect("compression failed");
        assert_eq!(
            actual,
            expected,
            "case {name}, quality {}, lgwin {lgwin}: {} bytes vs {} bytes",
            quality.get(),
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

#[test]
fn a_reused_compressor_matches_the_c_encoder_call_after_call() {
    // The whole point of a stateful compressor is that the second call reuses
    // the first one's workspace. It has to reach the same bytes doing so.
    let corpora = structural_corpora();
    for quality in IMPLEMENTED_QUALITIES {
        let mut encoder = encoder(quality, 22);
        for corpus in &corpora {
            let data = prefix_for(quality, &corpus.data);
            let expected = c_compress(quality_number(quality), 22, data);
            let actual = encoder.compress(data).expect("compression failed");
            assert_eq!(
                actual,
                expected,
                "case {}, quality {}: reuse left the reference",
                corpus.name,
                quality.get()
            );
        }
    }
}

#[test]
fn reconfiguring_between_calls_matches_the_c_encoder() {
    // One compressor walked across every quality, which is the workload the
    // retention policy exists for. Each configuration still has to reach the
    // bytes a compressor built only for it would.
    let data = structural_corpora()
        .into_iter()
        .find(|corpus| corpus.data.len() > 4096)
        .map(|corpus| corpus.data)
        .expect("a corpus large enough to be worth splitting");

    let mut encoder = encoder(IMPLEMENTED_QUALITIES[0], 22);
    for quality in IMPLEMENTED_QUALITIES {
        encoder
            .reconfigure(config(quality, 22))
            .expect("a legal configuration");
        let data = prefix_for(quality, &data);
        let expected = c_compress(quality_number(quality), 22, data);
        assert_eq!(
            encoder.compress(data).expect("compression failed"),
            expected,
            "quality {} differed after a reconfigure",
            quality.get()
        );
    }
}
