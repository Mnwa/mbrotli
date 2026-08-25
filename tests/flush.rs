//! `Write::flush` must make the stream readable without terminating it.
//!
//! Three properties are asserted, at every implemented quality:
//!
//! 1. everything written before a flush decodes out of the bytes produced so
//!    far, without the stream being finished;
//! 2. the finished stream still round-trips to the original input;
//! 3. the bytes are identical to what the C reference emits when it is driven
//!    with `BROTLI_OPERATION_FLUSH` at the same points.

mod support;

use mbrotli::Brotli;
use mbrotli::compressor::{CompressParams, QualityLevel};
use std::io::Write;
use support::{
    CParams, IMPLEMENTED_QUALITIES, c_compress_flushing, c_decompress, c_decompress_partial, params,
};

/// Window every case in this file uses.
const LGWIN: u8 = 22;

/// Compresses `chunks` through the writer, flushing after all but the last.
fn flush_between_chunks(chunks: &[&[u8]], parameters: CompressParams) -> Vec<u8> {
    let compressor = Brotli::default().compressor();
    let mut sink = compressor.compress_writer(parameters, Vec::new());
    for (index, chunk) in chunks.iter().enumerate() {
        sink.write_all(chunk).expect("write failed");
        if index + 1 != chunks.len() {
            sink.flush().expect("flush failed");
        }
    }
    sink.finish().expect("finish failed")
}

/// The C parameters matching [`params`], so the two encoders are comparable.
fn c_params(quality: QualityLevel) -> CParams {
    CParams::new(usize::from(quality) as std::ffi::c_int, LGWIN.into())
}

/// Chunk sets that put a flush in every interesting place.
fn chunk_sets() -> Vec<Vec<Vec<u8>>> {
    let long = b"the quick brown fox jumps over the lazy dog. ".repeat(400);
    vec![
        // Two ordinary chunks.
        vec![
            b"hello hello hello ".to_vec(),
            b"world world world".to_vec(),
        ],
        // A flush with nothing buffered after it.
        vec![b"payload payload payload".to_vec(), Vec::new()],
        // A flush before anything was ever written.
        vec![Vec::new(), b"payload payload payload".to_vec()],
        // Back-to-back flushes with no input in between.
        vec![
            b"first".to_vec(),
            Vec::new(),
            Vec::new(),
            b"second".to_vec(),
        ],
        // A single byte either side, where the padding dominates.
        vec![b"a".to_vec(), b"b".to_vec()],
        // Past one meta-block, so the flush lands mid-window.
        vec![long.clone(), long],
        // Incompressible bytes, which take the uncompressed meta-block path.
        vec![
            (0u8..=255).cycle().take(9000).collect(),
            (0u8..=255).rev().cycle().take(9000).collect(),
        ],
        // Nothing at all, twice.
        vec![Vec::new(), Vec::new()],
    ]
}

#[test]
fn flushed_prefix_decodes_before_the_stream_is_finished() {
    for quality in IMPLEMENTED_QUALITIES {
        for chunks in chunk_sets() {
            // Everything but the final chunk is written and then flushed, so a
            // decoder must be able to see it without the stream ending.
            let Some((_, head)) = chunks.split_last() else {
                continue;
            };
            let expected: Vec<u8> = head.concat();

            let compressor = Brotli::default().compressor();
            let mut sink = compressor.compress_writer(params(quality, LGWIN), Vec::new());
            for chunk in head {
                sink.write_all(chunk).expect("write failed");
            }
            sink.flush().expect("flush failed");
            let partial = sink.get_ref().clone();

            let decoded = c_decompress_partial(&partial, expected.len() + 1024)
                .expect("the decoder rejected a flushed prefix");
            assert_eq!(
                decoded, expected,
                "q{quality:?}: a flushed prefix did not decode to everything written"
            );
        }
    }
}

#[test]
fn flushing_does_not_break_the_finished_stream() {
    for quality in IMPLEMENTED_QUALITIES {
        for chunks in chunk_sets() {
            let borrowed: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
            let expected: Vec<u8> = chunks.concat();
            let compressed = flush_between_chunks(&borrowed, params(quality, LGWIN));

            let decoded = c_decompress(&compressed, expected.len().max(1))
                .expect("the decoder rejected a finished stream that had been flushed");
            assert_eq!(
                decoded, expected,
                "q{quality:?}: a flushed stream did not round-trip"
            );
        }
    }
}

#[test]
fn flushing_matches_the_reference_byte_for_byte() {
    for quality in IMPLEMENTED_QUALITIES {
        for chunks in chunk_sets() {
            let borrowed: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
            let ours = flush_between_chunks(&borrowed, params(quality, LGWIN));
            let theirs = c_compress_flushing(c_params(quality), &borrowed);
            assert_eq!(
                ours,
                theirs,
                "q{quality:?}: flushed output differed from the reference for chunks {:?}",
                chunks.iter().map(Vec::len).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn flushing_without_input_still_emits_the_header() {
    // The stream header is bits, not bytes, so a flush before any input has to
    // pad it out; a caller that flushes early must still get something.
    for quality in IMPLEMENTED_QUALITIES {
        let compressor = Brotli::default().compressor();
        let mut sink = compressor.compress_writer(params(quality, LGWIN), Vec::new());
        sink.flush().expect("flush failed");
        assert!(
            !sink.get_ref().is_empty(),
            "q{quality:?}: an early flush emitted nothing"
        );

        let theirs = c_compress_flushing(c_params(quality), &[&[][..], &[][..]]);
        let finished = sink.finish().expect("finish failed");
        assert_eq!(
            finished, theirs,
            "q{quality:?}: an early flush differed from the reference"
        );
    }
}

#[test]
fn a_flush_with_nothing_pending_is_idempotent() {
    // Two flushes in a row must not emit a second padding block: the stream is
    // already aligned, and the reference injects nothing in that case.
    for quality in IMPLEMENTED_QUALITIES {
        let compressor = Brotli::default().compressor();
        let mut sink = compressor.compress_writer(params(quality, LGWIN), Vec::new());
        sink.write_all(b"some payload worth compressing")
            .expect("write failed");
        sink.flush().expect("first flush failed");
        let after_one = sink.get_ref().len();
        sink.flush().expect("second flush failed");
        assert_eq!(
            sink.get_ref().len(),
            after_one,
            "q{quality:?}: a redundant flush wrote bytes"
        );
    }
}

#[test]
fn a_stream_that_is_never_flushed_is_unchanged() {
    // The flush path must not perturb the ordinary one: identical input with
    // no flush has to give exactly the bytes it always did.
    for quality in IMPLEMENTED_QUALITIES {
        let data = b"the quick brown fox jumps over the lazy dog. ".repeat(200);
        let compressor = Brotli::default().compressor();
        let mut sink = compressor.compress_writer(params(quality, LGWIN), Vec::new());
        sink.write_all(&data).expect("write failed");
        let streamed = sink.finish().expect("finish failed");

        let theirs = c_compress_flushing(c_params(quality), &[&data[..]]);
        assert_eq!(
            streamed, theirs,
            "q{quality:?}: an unflushed stream differed from the reference"
        );
    }
}
