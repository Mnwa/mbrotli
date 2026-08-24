//! Round-trip tests against an independent RFC 7932 decoder.
//!
//! The decoder is Google's C implementation, pinned to the same v1.2.0 commit
//! the encoder was ported from, and shares no code with this crate.

mod support;

use mbrotli::Brotli;
use support::{
    IMPLEMENTED_QUALITIES, boundary_corpora, c_decompress, host_levels, params, structural_corpora,
};

/// Compresses one input and requires the C decoder to recover it exactly.
fn assert_round_trips(name: &str, data: &[u8], lgwin: usize) {
    let compressor = Brotli::default().compressor();
    for quality in IMPLEMENTED_QUALITIES {
        let compressed = compressor
            .compress(params(quality, lgwin), data)
            .expect("compression failed");
        let decoded = c_decompress(&compressed, data.len())
            .unwrap_or_else(|| panic!("case {name}: the decoder rejected the stream"));
        assert_eq!(
            decoded.len(),
            data.len(),
            "case {name}, quality {}, lgwin {lgwin}: decoded length differs",
            usize::from(quality)
        );
        assert_eq!(
            decoded,
            data,
            "case {name}, quality {}, lgwin {lgwin}",
            usize::from(quality)
        );
    }
}

#[test]
fn structural_corpora_round_trip() {
    for corpus in structural_corpora() {
        assert_round_trips(&corpus.name, &corpus.data, 22);
    }
}

#[test]
fn boundary_lengths_round_trip() {
    for corpus in boundary_corpora() {
        assert_round_trips(&corpus.name, &corpus.data, 22);
    }
}

#[test]
fn every_window_size_round_trips() {
    let corpora = structural_corpora();
    for lgwin in 10..=24usize {
        for corpus in &corpora {
            assert_round_trips(&corpus.name, &corpus.data, lgwin);
        }
    }
}

#[test]
fn every_backend_round_trips() {
    let corpora = structural_corpora();
    for (level_name, level) in host_levels() {
        let compressor = Brotli::from(level).compressor();
        for corpus in &corpora {
            for quality in IMPLEMENTED_QUALITIES {
                let compressed = compressor
                    .compress(params(quality, 22), &corpus.data)
                    .expect("compression failed");
                let decoded = c_decompress(&compressed, corpus.data.len()).unwrap_or_else(|| {
                    panic!(
                        "{level_name}/{}: the decoder rejected the stream",
                        corpus.name
                    )
                });
                assert_eq!(decoded, corpus.data, "{level_name}/{}", corpus.name);
            }
        }
    }
}

#[test]
fn compression_is_deterministic() {
    let compressor = Brotli::default().compressor();
    for corpus in structural_corpora() {
        for quality in IMPLEMENTED_QUALITIES {
            let first = compressor
                .compress(params(quality, 22), &corpus.data)
                .expect("compression failed");
            let second = compressor
                .compress(params(quality, 22), &corpus.data)
                .expect("compression failed");
            assert_eq!(first, second, "case {}", corpus.name);
        }
    }
}

#[test]
fn output_stays_within_the_documented_bound() {
    let compressor = Brotli::default().compressor();
    for corpus in structural_corpora() {
        for quality in IMPLEMENTED_QUALITIES {
            let parameters = params(quality, 22);
            let bound = compressor
                .calculate_bound(&parameters, corpus.data.len())
                .expect("bound overflowed");
            let compressed = compressor
                .compress(parameters, &corpus.data)
                .expect("compression failed");
            assert!(
                compressed.len() <= bound,
                "case {}: {} bytes exceed the bound of {bound}",
                corpus.name,
                compressed.len()
            );
        }
    }
}
