#![cfg(feature = "decompression")]
#[path = "decode_support/c_encoder.rs"]
pub mod c_encoder;
mod support;
use c_encoder::Encoder;
use google_brotli_ffi as ffi;
use mbrotli::{DecoderConfig, Decompressor};

#[test]
fn c_parameter_cartesian_matrix_includes_extended_headers() {
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let payload = b"\0\xffTEXT text text ffi, JSON {\"one\":1}.".repeat(8);
    for large in [false, true] {
        for quality in 0..=11 {
            for mode in 0..=2 {
                for window in 10..=if large { 30 } else { 24 } {
                    let mut c = Encoder::new(&[
                        (ffi::BROTLI_PARAM_QUALITY, quality),
                        (ffi::BROTLI_PARAM_MODE, mode),
                        (ffi::BROTLI_PARAM_LARGE_WINDOW, u32::from(large)),
                        (ffi::BROTLI_PARAM_LGWIN, window),
                    ]);
                    let compressed = c.push(&payload, ffi::BROTLI_OPERATION_FINISH, 17, true);
                    assert_eq!(decoder.decompress(&compressed).unwrap_or_else(|error|panic!("large={large}, quality={quality}, mode={mode}, lgwin={window}: {error}")),payload);
                    assert_eq!(
                        support::c_decompress_large_window(&compressed, payload.len()).unwrap(),
                        payload
                    );
                }
            }
        }
    }
}

#[test]
fn c_block_sizes_hints_and_metadata_schedules_are_not_payload_assumptions() {
    let source = b"three byte patterns and metadata in streaming input.\0\xff".repeat(2000);
    for lgblock in [0, 16, 17, 18, 19, 20, 21, 22, 23, 24] {
        for hint in [0, 1, 999_999] {
            let mut c = Encoder::new(&[
                (ffi::BROTLI_PARAM_QUALITY, 5),
                (ffi::BROTLI_PARAM_LGBLOCK, lgblock),
                (ffi::BROTLI_PARAM_SIZE_HINT, hint),
            ]);
            let mut compressed = Vec::new();
            for (i, chunk) in source.chunks(1024).enumerate() {
                compressed.extend(c.push(chunk, ffi::BROTLI_OPERATION_PROCESS, 1, i % 2 == 0));
                compressed.extend(c.push(&[], ffi::BROTLI_OPERATION_FLUSH, 3, true));
                compressed.extend(c.push(&[], ffi::BROTLI_OPERATION_FLUSH, 8, false));
                compressed.extend(c.push(
                    b"ignored metadata",
                    ffi::BROTLI_OPERATION_EMIT_METADATA,
                    7,
                    true,
                ));
                compressed.extend(c.push(&[], ffi::BROTLI_OPERATION_EMIT_METADATA, 1, false));
            }
            compressed.extend(c.push(&[], ffi::BROTLI_OPERATION_FINISH, 1, true));
            assert_eq!(
                Decompressor::new(DecoderConfig::default())
                    .unwrap()
                    .decompress(&compressed)
                    .unwrap(),
                source
            );
            assert_eq!(
                support::c_decompress(&compressed, source.len()).unwrap(),
                source
            );
        }
    }
}

#[test]
fn complete_c_continuations_and_golden_provenance() {
    for large in [false, true] {
        for tail in [
            b"".as_slice(),
            b"x",
            b"xy",
            b"continuation continuation continuation",
        ] {
            let parameters = [
                (ffi::BROTLI_PARAM_QUALITY, 5),
                (ffi::BROTLI_PARAM_LARGE_WINDOW, u32::from(large)),
                (ffi::BROTLI_PARAM_LGWIN, if large { 25 } else { 10 }),
            ];
            let prefix = b"a flushed predecessor with byte history";
            let mut c = Encoder::new(&parameters);
            let mut compressed = c.push(prefix, ffi::BROTLI_OPERATION_FLUSH, 3, true);
            let mut next_parameters = parameters.to_vec();
            next_parameters.push((ffi::BROTLI_PARAM_STREAM_OFFSET, prefix.len() as u32));
            let mut next = Encoder::new(&next_parameters);
            compressed.extend(next.push(tail, ffi::BROTLI_OPERATION_FINISH, 7, false));
            let payload = [prefix.as_slice(), tail].concat();
            assert_eq!(
                Decompressor::new(DecoderConfig::default())
                    .unwrap()
                    .decompress(&compressed)
                    .unwrap(),
                payload
            );
            assert_eq!(
                support::c_decompress_large_window(&compressed, payload.len()).unwrap(),
                payload
            );
            if tail.len() > 2 {
                let name = if large {
                    "continuation-large"
                } else {
                    "continuation-standard"
                };
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/decompress");
                if std::env::var_os("MBROTLI_WRITE_FIXTURES").is_some() {
                    std::fs::write(path.join(format!("{name}.br")), &compressed).unwrap();
                    std::fs::write(path.join(format!("{name}.raw")), &payload).unwrap();
                } else {
                    assert_eq!(
                        std::fs::read(path.join(format!("{name}.br"))).unwrap(),
                        compressed
                    );
                    assert_eq!(
                        std::fs::read(path.join(format!("{name}.raw"))).unwrap(),
                        payload
                    );
                }
            }
        }
    }
}

#[test]
fn historical_official_c_fixtures_remain_decodable() {
    let root = support::vendor_testdata_dir();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if let Some((payload_name, _)) = name.split_once(".compressed") {
            let compressed = std::fs::read(&path).unwrap();
            let payload = std::fs::read(root.join(payload_name)).unwrap();
            assert_eq!(
                decoder
                    .decompress(&compressed)
                    .unwrap_or_else(|error| panic!("{name}: {error}")),
                payload,
                "{name}"
            );
        }
    }
}

/// A copy that pauses on a full output window resumes in the byte-exact
/// `Stage::Copy`, which must not lose the distance-cache push the fast path
/// made for it: `zerosukkanooa` follows a long explicit-distance copy with
/// short-code distances, so a stale cache silently produces wrong bytes.
#[test]
fn output_pauses_inside_copies_keep_the_distance_cache() {
    use mbrotli::{DecodeOperation, DecodeStreamConfig, DecoderStatus};
    let compressed = support::vendor_file("zerosukkanooa.compressed");
    let payload = support::vendor_file("zerosukkanooa");
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    for (input_chunk, output_chunk) in [(4096, 16), (compressed.len(), 1), (64, 7)] {
        let mut output = vec![0u8; payload.len()];
        let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
        let (mut read, mut written) = (0, 0);
        loop {
            let end = (read + input_chunk).min(compressed.len());
            let output_end = (written + output_chunk).min(output.len());
            let operation = if end == compressed.len() {
                DecodeOperation::Finish
            } else {
                DecodeOperation::Process
            };
            let progress = session
                .process(
                    &compressed[read..end],
                    &mut output[written..output_end],
                    operation,
                )
                .unwrap();
            read += progress.consumed;
            written += progress.produced;
            if progress.status == DecoderStatus::Finished {
                break;
            }
            assert!(progress.consumed != 0 || progress.produced != 0, "stalled");
        }
        assert_eq!(
            written,
            payload.len(),
            "chunks {input_chunk}/{output_chunk}"
        );
        let first = output.iter().zip(&payload).position(|(a, b)| a != b);
        assert_eq!(first, None, "chunks {input_chunk}/{output_chunk} differ");
    }
}

#[test]
fn byte_corpora_cover_small_lengths_and_storage_boundaries() {
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let mut rng = support::Rng::new(0xdec0de);
    let mut lengths = support::boundary_lengths();
    lengths.extend([1023, 1024, 1025]);
    for length in lengths {
        for alphabet in [1, 2, 3, 256] {
            let source = rng.bytes(length, alphabet);
            for quality in [0, 5, 11] {
                let compressed = support::c_compress_native_one_shot(quality, 10, &source);
                assert_eq!(decoder.decompress(&compressed).unwrap(), source);
            }
        }
    }
}

#[test]
fn multiple_c_restarts_form_one_member() {
    let chunks: [&[u8]; 5] = [
        b"prefix prefix prefix",
        b"",
        b"x",
        b"xy",
        b"final continuation",
    ];
    let mut compressed = Vec::new();
    let mut offset = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        let mut c = Encoder::new(&[
            (ffi::BROTLI_PARAM_QUALITY, 5),
            (ffi::BROTLI_PARAM_LGWIN, 10),
            (ffi::BROTLI_PARAM_STREAM_OFFSET, offset),
        ]);
        compressed.extend(c.push(
            chunk,
            if index + 1 == chunks.len() {
                ffi::BROTLI_OPERATION_FINISH
            } else {
                ffi::BROTLI_OPERATION_FLUSH
            },
            3,
            true,
        ));
        offset += chunk.len() as u32;
    }
    let expected = chunks.concat();
    assert_eq!(
        Decompressor::new(DecoderConfig::default())
            .unwrap()
            .decompress(&compressed)
            .unwrap(),
        expected
    );
    assert_eq!(
        support::c_decompress(&compressed, expected.len()).unwrap(),
        expected
    );
}

#[cfg(not(feature = "no_std"))]
#[test]
#[cfg(feature = "compression")]
fn assembled_parallel_encoder_output_decodes_as_one_member() {
    use mbrotli::compressor::parallel::{
        BatchConfig, ParallelCompressor, ParallelConfig, SegmentSize, TaskCount,
    };
    let input = b"parallel parts preserve the common Brotli stream state. ".repeat(5000);
    let config =
        ParallelConfig::from(SegmentSize::try_from(65536).unwrap()).with_minimum_parallel_size(0);
    let mut compressor = ParallelCompressor::new(
        mbrotli::EncoderConfig::default().with_quality(mbrotli::Quality::Q5),
        config,
    )
    .unwrap();
    let mut batch = compressor
        .prepare_slice(
            &input,
            BatchConfig::memory(TaskCount::try_from(4).unwrap(), 32 << 20),
        )
        .unwrap();
    batch.run_inline().unwrap();
    let mut compressed = Vec::new();
    batch.finish_into(&mut compressed).unwrap();
    assert_eq!(
        support::c_decompress(&compressed, input.len()).unwrap(),
        input
    );
    if std::env::var_os("MBROTLI_WRITE_FIXTURES").is_some() {
        std::fs::write("tests/fixtures/decompress/parallel.br", &compressed).unwrap();
    }
    assert_eq!(
        Decompressor::new(DecoderConfig::default())
            .unwrap()
            .decompress(&compressed)
            .unwrap(),
        input
    );
}

#[test]
fn c_continuations_keep_ordered_raw_dictionary_attachments() {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let first = b"ordered dictionary prefix with unique words and punctuation! ".repeat(8);
    let second = b"another prefix segment used by independently restarted encoders ".repeat(8);
    let chunks = [&first[3..123], &second[9..178], &first[77..200]];
    let dictionary = DecodeDictionary::new(
        &[
            DictionaryAttachment::Raw(&first),
            DictionaryAttachment::Raw(&second),
        ],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    for large in [false, true] {
        let mut compressed = Vec::new();
        let mut offset = 0;
        for (index, chunk) in chunks.iter().enumerate() {
            let mut encoder = Encoder::new(&[
                (ffi::BROTLI_PARAM_QUALITY, 5),
                (ffi::BROTLI_PARAM_LARGE_WINDOW, u32::from(large)),
                (ffi::BROTLI_PARAM_LGWIN, if large { 25 } else { 10 }),
                (ffi::BROTLI_PARAM_STREAM_OFFSET, offset),
            ]);
            // C 1.2.0's TakeOutput-only path can leave the continuation flint
            // pending; caller buffers drain the unchanged FLUSH/FINISH schedule.
            encoder.attach(&first);
            encoder.attach(&second);
            compressed.extend(encoder.push(
                chunk,
                if index + 1 == chunks.len() {
                    ffi::BROTLI_OPERATION_FINISH
                } else {
                    ffi::BROTLI_OPERATION_FLUSH
                },
                3,
                false,
            ));
            offset += chunk.len() as u32;
        }
        let expected = chunks.concat();
        assert_eq!(
            support::c_decompress_with_prefixes(&[&first, &second], &compressed, expected.len())
                .unwrap(),
            expected
        );
        assert_eq!(
            Decompressor::new(DecoderConfig::default())
                .unwrap()
                .decompress_with_dictionary(&dictionary, &compressed)
                .unwrap(),
            expected
        );
    }
}
