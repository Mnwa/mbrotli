mod support;

use mbrotli::{
    DecodeError, DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus, Decompressor,
    MemberMode,
};

#[test]
fn native_c_streams_decode_for_every_quality_and_window() {
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let corpora = [
        Vec::new(),
        b"a".to_vec(),
        b"compression dictionaries transform the information into bytes".repeat(40),
        (0..8192).map(|i| (i * 173 % 251) as u8).collect(),
    ];
    for quality in 0..=11 {
        for window in 10..=24 {
            for source in &corpora {
                let compressed = support::c_compress_native_one_shot(quality, window, source);
                let decoded = decoder.decompress(&compressed).unwrap_or_else(|e| {
                    panic!(
                        "quality {quality}, window {window}, length {}: {e}",
                        source.len()
                    )
                });
                assert_eq!(&decoded, source, "quality {quality}, window {window}");
                let mut exact = vec![0; source.len()];
                assert_eq!(
                    decoder
                        .decompress_to_slice(&compressed, &mut exact)
                        .unwrap(),
                    source.len()
                );
                assert_eq!(&exact, source);
            }
        }
    }
}

#[test]
fn one_byte_chunks_resume_without_consuming_the_next_member() {
    let source =
        b"Dictionary transforms and overlapping repeated repeated repeated bytes.".repeat(20);
    let compressed = support::c_compress_native_one_shot(11, 10, &source);
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
    let mut cursor = 0;
    let mut result = Vec::new();
    loop {
        let end = (cursor + 1).min(compressed.len());
        let mut output = [0];
        let progress = session
            .process(
                &compressed[cursor..end],
                &mut output,
                DecodeOperation::Process,
            )
            .unwrap();
        cursor += progress.consumed;
        result.extend_from_slice(&output[..progress.produced]);
        if progress.status == DecoderStatus::Finished {
            break;
        }
        assert!(progress.consumed != 0 || progress.produced != 0);
    }
    assert_eq!(result, source);
    assert_eq!(cursor, compressed.len());
    assert_eq!(session.members_decoded(), 1);
    assert_eq!(
        session
            .process(b"tail", &mut [0], DecodeOperation::Process)
            .unwrap()
            .consumed,
        0
    );
}

#[test]
fn malformed_operation_rolls_back_and_allows_reuse() {
    let source = b"Testing recovery after a truncated complex Huffman description.".repeat(8);
    let compressed = support::c_compress_native_one_shot(11, 22, &source);
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    for end in 0..compressed.len() {
        let mut output = b"prefix".to_vec();
        assert!(
            decoder
                .decompress_into(&compressed[..end], &mut output)
                .is_err()
        );
        assert_eq!(output, b"prefix");
        assert_eq!(
            decoder.decompress(&compressed).unwrap(),
            source,
            "truncated at {end}"
        );
    }
}

#[test]
fn strict_and_concatenated_operations_handle_member_boundaries() {
    let first = support::c_compress_native_one_shot(5, 22, b"first");
    let second = support::c_compress_native_one_shot(1, 10, b"second");
    let combined = [first.as_slice(), second.as_slice()].concat();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert!(
        matches!(decoder.decompress(&combined), Err(DecodeError::TrailingData { offset }) if offset == first.len() as u64)
    );
    decoder
        .reconfigure(DecoderConfig::default().with_member_mode(MemberMode::Concatenated))
        .unwrap();
    assert_eq!(decoder.decompress(&combined).unwrap(), b"firstsecond");
    let large = include_bytes!("fixtures/decompress/continuation-large.br");
    let large_payload = include_bytes!("fixtures/decompress/continuation-large.raw");
    let mixed = [
        first.as_slice(),
        &[0x3b],
        large.as_slice(),
        second.as_slice(),
        &[0x3b],
    ]
    .concat();
    let expected = [b"first".as_slice(), large_payload.as_slice(), b"second"].concat();
    let mut output = vec![0; expected.len()];
    {
        let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
        let progress = session
            .process(&mixed, &mut output, DecodeOperation::Finish)
            .unwrap();
        assert_eq!(progress.status, DecoderStatus::Finished);
        assert_eq!(session.members_decoded(), 5);
        assert_eq!(output, expected);
    }
    let config = *decoder.config();
    for limit in [expected.len() - 1, expected.len(), expected.len() + 1] {
        decoder
            .reconfigure(config.with_limits(
                mbrotli::DecodeLimits::default().with_max_output_bytes(Some(limit as u64)),
            ))
            .unwrap();
        let result = decoder.decompress(&mixed);
        if limit < expected.len() {
            assert!(matches!(
                result,
                Err(DecodeError::OutputLimitExceeded { .. })
            ));
        } else {
            assert_eq!(result.unwrap(), expected);
        }
    }
    decoder.reconfigure(config).unwrap();
    assert!(matches!(
        decoder.decompress(&mixed[..mixed.len() - 2]),
        Err(DecodeError::UnexpectedEndOfInput)
    ));

    assert!(matches!(
        decoder.decompress(&[]),
        Err(DecodeError::UnexpectedEndOfInput)
    ));
}

#[test]
fn raw_prefixes_decode_from_both_dictionary_representations() {
    use mbrotli::dictionary::{
        DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment, DictionaryBuilder,
    };
    let prefix = b"Content-Type: application/json\r\nX-Request-ID: 0123456789abcdef\r\n";
    let second = b"another prefix with arbitrary \0\xff bytes";
    let owned = DecodeDictionary::new(
        &[
            DictionaryAttachment::Raw(prefix),
            DictionaryAttachment::Raw(second),
        ],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    let prepared = DictionaryBuilder::new()
        .add_prefix(&prefix[..])
        .add_prefix(&second[..])
        .build()
        .unwrap();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let payload = [prefix.as_slice(), second.as_slice(), prefix.as_slice()].concat();
    for quality in 2..=11 {
        let compressed = support::c_compress_with_prefixes(
            support::CParams::new(quality, 22),
            &[prefix, second],
            &payload,
        );
        assert_eq!(
            decoder
                .decompress_with_dictionary(&owned, &compressed)
                .unwrap(),
            payload
        );
        assert_eq!(
            decoder
                .decompress_with_dictionary(&prepared, &compressed)
                .unwrap(),
            payload
        );
        let mut slice = vec![0; payload.len()];
        assert_eq!(
            decoder
                .decompress_with_dictionary_to_slice(&owned, &compressed, &mut slice)
                .unwrap(),
            payload.len()
        );
        assert_eq!(slice, payload);
        let mut vec = b"prefix".to_vec();
        assert_eq!(
            decoder
                .decompress_with_dictionary_into(&owned, &compressed, &mut vec)
                .unwrap(),
            6..6 + payload.len()
        );
        assert_eq!(&vec[6..], payload);
        let mut session = decoder
            .start_with_dictionary(&owned, DecodeStreamConfig::default())
            .unwrap();
        assert_eq!(
            session
                .process(&compressed, &mut slice, DecodeOperation::Finish)
                .unwrap()
                .status,
            DecoderStatus::Finished
        );
    }
    let compressed = support::c_compress_with_prefixes(
        support::CParams::new(5, 22),
        &[prefix, second],
        &payload,
    );
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let (owned, prepared, compressed, payload) = (&owned, &prepared, &compressed, &payload);
            scope.spawn(move || {
                let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
                for _ in 0..4 {
                    let view: mbrotli::dictionary::DictionaryRef<'_> = if worker % 2 == 0 {
                        owned.into()
                    } else {
                        prepared.into()
                    };
                    assert_eq!(
                        decoder
                            .decompress_with_dictionary(view, compressed)
                            .unwrap(),
                        *payload
                    );
                }
            });
        }
    });
    assert_eq!(owned.attachment_count(), 2);
    assert_eq!(owned.retained_bytes(), prefix.len() + second.len());
}

#[test]
fn streaming_c_parameters_and_flush_boundaries_decode() {
    let source = b"font text binary\0\xff\x80 signed UTF-8: \xc3\xa9\n".repeat(100);
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    for mode in [
        google_brotli_ffi::BROTLI_MODE_GENERIC,
        google_brotli_ffi::BROTLI_MODE_TEXT,
        google_brotli_ffi::BROTLI_MODE_FONT,
    ] {
        for postfix in 0..=3 {
            for direct in 0..=15 {
                let mut params = support::CParams::new(7, 10);
                params.mode = mode;
                params.npostfix = postfix;
                params.ndirect = direct << postfix;
                params.disable_literal_context_modeling = direct % 2 == 0;
                let compressed = support::c_compress_with(params, &source);
                assert_eq!(decoder.decompress(&compressed).unwrap(), source);
            }
        }
    }
    let chunks: Vec<&[u8]> = source.chunks(51).collect();
    let compressed = support::c_compress_flushing(support::CParams::new(5, 10), &chunks);
    assert_eq!(decoder.decompress(&compressed).unwrap(), source);
}

#[test]
fn limits_exact_sizes_and_abandoned_sessions_are_enforced() {
    use mbrotli::{DecodeLimits, OutputSize, RetentionPolicy};
    let compressed = support::c_compress_native_one_shot(5, 22, b"payload");
    let config = DecoderConfig::default()
        .with_limits(DecodeLimits::default().with_max_output_bytes(Some(7)));
    let mut decoder = Decompressor::builder(config)
        .with_retention(RetentionPolicy::Aggressive)
        .with_backend(mbrotli::Backend::default())
        .build()
        .unwrap();
    assert_eq!(decoder.retained_bytes(), 0);
    {
        let mut session = decoder.start(OutputSize::Exact(7).into()).unwrap();
        assert_eq!(
            session
                .process(&compressed, &mut [0; 7], DecodeOperation::Finish)
                .unwrap()
                .status,
            DecoderStatus::Finished
        );
        assert_eq!(session.total_in(), compressed.len() as u64);
        assert_eq!(session.total_out(), 7);
        assert!(session.window().is_some());
        assert!(session.is_finished());
    }
    let retained = decoder.retained_bytes();
    assert!(retained > 0);
    assert_eq!(decoder.decompress(&compressed).unwrap(), b"payload");
    assert_eq!(decoder.retained_bytes(), retained);
    assert!(matches!(
        decoder.start(OutputSize::Exact(8).into()),
        Err(DecodeError::OutputLimitExceeded { limit: 7 })
    ));
    std::mem::forget(decoder.start(DecodeStreamConfig::default()).unwrap());
    decoder.trim(RetentionPolicy::ReleaseAll);
    assert!(matches!(
        decoder.decompress(&compressed),
        Err(DecodeError::AbandonedSession)
    ));
    decoder.recover();
    assert_eq!(decoder.retained_bytes(), 0);
    assert_eq!(decoder.fork_empty().config(), decoder.config());
    assert_eq!(decoder.decompress(&compressed).unwrap(), b"payload");
    assert!(matches!(
        decoder.decompress_to_slice(&compressed, &mut [0; 6]),
        Err(DecodeError::OutputTooSmall { written: 6 })
    ));
}
