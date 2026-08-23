//! Streaming adapters: chunk boundaries must not change the stream.

mod support;

use mbrotli::Brotli;
use std::io::{Read, Write};
use support::{FAST_QUALITIES, c_decompress, params, structural_corpora};

/// Compresses `data` through the writer adapter using fixed-size chunks.
fn compress_with_writer(data: &[u8], chunk: usize, quality_index: usize, lgwin: usize) -> Vec<u8> {
    let compressor = Brotli::default().compressor();
    let parameters = params(FAST_QUALITIES[quality_index], lgwin);
    let mut sink = compressor.compress_writer(parameters, Vec::new());
    for piece in data.chunks(chunk.max(1)) {
        sink.write_all(piece).expect("write failed");
    }
    sink.finish().expect("finish failed")
}

/// Compresses `data` through the reader adapter using fixed-size reads.
fn compress_with_reader(data: &[u8], chunk: usize, quality_index: usize, lgwin: usize) -> Vec<u8> {
    let compressor = Brotli::default().compressor();
    let parameters = params(FAST_QUALITIES[quality_index], lgwin);
    let mut source = compressor.compress_reader(parameters, data);
    let mut output = Vec::new();
    let mut buffer = vec![0u8; chunk.max(1)];
    loop {
        let count = source.read(&mut buffer).expect("read failed");
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn writer_output_is_independent_of_the_chunk_size() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 61) as u8).collect();
    for quality_index in 0..FAST_QUALITIES.len() {
        let reference = compress_with_writer(&data, data.len(), quality_index, 16);
        for chunk in [1usize, 3, 1024, 65_536, 65_537, 131_072] {
            let actual = compress_with_writer(&data, chunk, quality_index, 16);
            assert_eq!(actual, reference, "chunk {chunk}, quality {quality_index}");
        }
    }
}

#[test]
fn reader_output_is_independent_of_the_read_size() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 61) as u8).collect();
    for quality_index in 0..FAST_QUALITIES.len() {
        let reference = compress_with_reader(&data, 1 << 20, quality_index, 16);
        for chunk in [1usize, 7, 4096, 65_536] {
            let actual = compress_with_reader(&data, chunk, quality_index, 16);
            assert_eq!(actual, reference, "chunk {chunk}, quality {quality_index}");
        }
    }
}

#[test]
fn writer_and_reader_agree_with_each_other() {
    for corpus in structural_corpora() {
        for quality_index in 0..FAST_QUALITIES.len() {
            let written = compress_with_writer(&corpus.data, 4096, quality_index, 18);
            let read = compress_with_reader(&corpus.data, 4096, quality_index, 18);
            assert_eq!(written, read, "case {}", corpus.name);
        }
    }
}

#[test]
fn streaming_matches_one_shot_for_inputs_past_the_fallback() {
    let compressor = Brotli::default().compressor();
    for corpus in structural_corpora() {
        // Tiny inputs can take the one-shot uncompressed fallback, which the
        // streaming API deliberately does not apply.
        if corpus.data.len() < 1024 {
            continue;
        }
        for (index, quality) in FAST_QUALITIES.into_iter().enumerate() {
            let one_shot = compressor
                .compress(params(quality, 18), &corpus.data)
                .expect("compression failed");
            let streamed = compress_with_writer(&corpus.data, 4096, index, 18);
            assert_eq!(streamed, one_shot, "case {}", corpus.name);
        }
    }
}

#[test]
fn streamed_output_round_trips() {
    for corpus in structural_corpora() {
        for quality_index in 0..FAST_QUALITIES.len() {
            for chunk in [1usize, 4096] {
                if chunk == 1 && corpus.data.len() > 4096 {
                    continue;
                }
                let compressed = compress_with_writer(&corpus.data, chunk, quality_index, 16);
                let decoded = c_decompress(&compressed, corpus.data.len()).unwrap_or_else(|| {
                    panic!("case {}: the decoder rejected the stream", corpus.name)
                });
                assert_eq!(decoded, corpus.data, "case {}", corpus.name);
            }
        }
    }
}

#[test]
fn empty_streams_are_valid() {
    for quality_index in 0..FAST_QUALITIES.len() {
        let written = compress_with_writer(&[], 1, quality_index, 22);
        assert!(!written.is_empty());
        assert_eq!(c_decompress(&written, 1), Some(Vec::new()));

        let read = compress_with_reader(&[], 1, quality_index, 22);
        assert_eq!(read, written);
    }
}
