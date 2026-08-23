//! The encoder checked against Google Brotli's own test corpus.
//!
//! `brotli-ffi/vendor/brotli/tests/testdata` is the corpus the reference
//! implementation tests itself with: Canterbury text, binary blobs, already
//! compressed payloads, long zero runs and back-reference edge cases. Running
//! the same files through both encoders is the strongest differential signal
//! available without inventing new data.

mod support;

use mbrotli::Brotli;
use support::{
    FAST_QUALITIES, c_compress, c_decompress, host_levels, params, quality_number, vendor_corpora,
    vendor_file,
};

/// Largest file size the per-corpus tests use, to keep debug runs quick.
const MAX_CORPUS_BYTES: usize = 1 << 20;

#[test]
fn vendor_corpus_matches_the_c_encoder() {
    let compressor = Brotli::default().compressor();
    for corpus in vendor_corpora(MAX_CORPUS_BYTES) {
        for quality in FAST_QUALITIES {
            for lgwin in [10usize, 16, 18, 22, 24] {
                let expected = c_compress(quality_number(quality), lgwin as i32, &corpus.data);
                let actual = compressor
                    .compress(params(quality, lgwin), &corpus.data)
                    .expect("compression failed");
                assert_eq!(
                    actual,
                    expected,
                    "{}: quality {}, lgwin {lgwin}",
                    corpus.name,
                    usize::from(quality)
                );
            }
        }
    }
}

#[test]
fn vendor_corpus_round_trips() {
    let compressor = Brotli::default().compressor();
    for corpus in vendor_corpora(MAX_CORPUS_BYTES) {
        for quality in FAST_QUALITIES {
            let compressed = compressor
                .compress(params(quality, 22), &corpus.data)
                .expect("compression failed");
            let decoded = c_decompress(&compressed, corpus.data.len())
                .unwrap_or_else(|| panic!("{}: the decoder rejected the stream", corpus.name));
            assert_eq!(decoded, corpus.data, "{}", corpus.name);
        }
    }
}

#[test]
fn vendor_corpus_agrees_across_backends() {
    let levels = host_levels();
    for corpus in vendor_corpora(MAX_CORPUS_BYTES) {
        for quality in FAST_QUALITIES {
            let mut reference: Option<Vec<u8>> = None;
            for &(level_name, level) in &levels {
                let actual = Brotli::from(level)
                    .compressor()
                    .compress(params(quality, 22), &corpus.data)
                    .expect("compression failed");
                match &reference {
                    None => reference = Some(actual),
                    Some(expected) => {
                        assert_eq!(&actual, expected, "{}: backend {level_name}", corpus.name);
                    }
                }
            }
        }
    }
}

#[test]
fn multi_fragment_input_matches_the_c_encoder() {
    // `bb.binast` is 12 MiB, so a 22 bit window splits it into several
    // fragments and exercises the carry of the trailing partial byte.
    let data = vendor_file("bb.binast");
    assert!(data.len() > 4 << 20, "expected a multi-fragment input");
    let compressor = Brotli::default().compressor();
    for quality in FAST_QUALITIES {
        for lgwin in [18usize, 22, 24] {
            let expected = c_compress(quality_number(quality), lgwin as i32, &data);
            let actual = compressor
                .compress(params(quality, lgwin), &data)
                .expect("compression failed");
            assert_eq!(
                actual,
                expected,
                "bb.binast: quality {}, lgwin {lgwin}",
                usize::from(quality)
            );
            let decoded = c_decompress(&actual, data.len()).expect("the decoder rejected it");
            assert_eq!(decoded, data);
        }
    }
}
