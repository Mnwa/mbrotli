#![cfg(feature = "decompression")]
mod support;
#[path = "decode_support/wire.rs"]
mod wire;
use mbrotli::{
    DecodeLimits, DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus, Decompressor,
    WindowLimit,
};
use wire::Wire;

#[test]
fn all_window_headers_decode_tiny_payloads_without_window_sized_allocations() {
    for large in [false, true] {
        for bits in 10..=if large { 62 } else { 24 } {
            let mut wire = Wire::window(bits, large);
            wire.metadata(&[]);
            wire.raw(b"xyz");
            wire.copy(9, 3, 9);
            let compressed = wire.finish();
            let mut decoder = Decompressor::new(
                DecoderConfig::default()
                    .with_limits(DecodeLimits::default().with_max_workspace_bytes(Some(16384))),
            )
            .unwrap();
            assert_eq!(decoder.decompress(&compressed).unwrap(), b"xyzxyzxyzxyz");
            assert!(decoder.retained_bytes() < 16384);
            if bits <= 30 {
                assert_eq!(
                    support::c_decompress_large_window(&compressed, 12).unwrap(),
                    b"xyzxyzxyzxyz"
                );
            }
        }
    }
}

#[test]
fn raw_prefix_reference_crosses_attachments_then_overlaps_history() {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let dictionary = DecodeDictionary::new(
        &[
            DictionaryAttachment::Raw(b"x"),
            DictionaryAttachment::Raw(b"y"),
        ],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    let mut wire = Wire::window(10, false);
    wire.raw(b"abcd");
    wire.copy(15, 6, 15);
    let compressed = wire.finish();
    // RFC 9841 section 3.2 explicitly permits crossing into history. Pinned
    // C 1.2.0 rejects address + length beyond the prefix in
    // InitializeCompoundDictionaryCopy, so it is not an oracle for this case.
    assert!(support::c_decompress_with_prefixes(&[b"x", b"y"], &compressed, 19).is_none());
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert_eq!(
        decoder
            .decompress_with_dictionary(&dictionary, &compressed)
            .unwrap(),
        b"abcdxyabcdxyabcdxya"
    );
}

/// An implicit distance repeats a prefix reference. The fast path pops the
/// cache for the implicit distance and must push it back before handing the
/// reference to the byte-exact `Stage::Resolve`, whatever follows it.
#[test]
fn implicit_distance_can_repeat_a_prefix_reference() {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Raw(b"wxyz")],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    let mut wire = Wire::window(10, false);
    wire.raw(b"abcd");
    // Distance 8 from position 4 starts the prefix; repeated implicitly from
    // position 6 it selects the prefix tail, and from position 8 the history.
    wire.copy(2, 8, 2);
    wire.copy_implicit(2, 2);
    wire.copy_implicit(9, 9);
    // Trailing metadata keeps whole-word refills possible through the last
    // command, so the fast path, not only the byte-exact stages, sees it.
    wire.metadata(&[7; 64]);
    let compressed = wire.finish();
    let expected = b"abcdwxyzabcdwxyza";
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert_eq!(
        decoder
            .decompress_with_dictionary(&dictionary, &compressed)
            .unwrap(),
        expected
    );
    assert_eq!(
        support::c_decompress_with_prefixes(&[b"wxyz"], &compressed, expected.len()).as_deref(),
        Some(&expected[..])
    );
}

#[test]
fn metadata_does_not_count_as_output_and_padding_remains_required() {
    let mut wire = Wire::window(16, false);
    wire.metadata(&[]);
    wire.metadata(&vec![42; 1025]);
    wire.metadata(&[]);
    let compressed = wire.finish();
    let config = DecoderConfig::default().with_limits(
        DecodeLimits::default()
            .with_max_output_bytes(Some(0))
            .with_max_workspace_bytes(Some(0)),
    );
    let mut decoder = Decompressor::new(config).unwrap();
    assert!(decoder.decompress(&compressed).unwrap().is_empty());
    assert_eq!(decoder.retained_bytes(), 0);
    assert!(support::c_decompress(&compressed, 0).unwrap().is_empty());
    let mut truncated = compressed;
    truncated.pop();
    assert!(decoder.decompress(&truncated).is_err());
}

#[test]
fn window_policy_is_checked_before_payload_allocation() {
    let mut decoder = Decompressor::new(
        DecoderConfig::default().with_window_limit(WindowLimit::standard(22).unwrap()),
    )
    .unwrap();
    assert!(matches!(
        decoder.decompress(&Wire::window(10, true).finish()),
        Err(mbrotli::DecodeError::LargeWindowDisabled)
    ));
    assert!(matches!(
        decoder.decompress(&Wire::window(24, false).finish()),
        Err(mbrotli::DecodeError::WindowLimitExceeded {
            declared: 24,
            allowed: 22
        })
    ));
}

#[test]
fn every_small_split_and_output_capacity_preserves_members() {
    let mut wire = Wire::window(16, false);
    wire.metadata(b"meta");
    wire.raw(b"abc");
    wire.copy(32, 3, 32);
    let compressed = wire.finish();
    let expected = support::c_decompress(&compressed, 35).unwrap();
    let cases = [
        (compressed.as_slice(), expected.as_slice()),
        (
            include_bytes!("fixtures/decompress/official-cli.br").as_slice(),
            include_bytes!("fixtures/decompress/continuation-standard.raw").as_slice(),
        ),
        (
            include_bytes!("fixtures/decompress/continuation-standard.br").as_slice(),
            include_bytes!("fixtures/decompress/continuation-standard.raw").as_slice(),
        ),
        (
            include_bytes!("fixtures/decompress/continuation-large.br").as_slice(),
            include_bytes!("fixtures/decompress/continuation-large.raw").as_slice(),
        ),
    ];
    for (compressed, expected) in cases {
        for split in 0..=compressed.len() {
            for capacity in [1, 2, 3, 7, 8, 15, 16, 31, 32, 63, 64, 127] {
                let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
                let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
                let mut cursor = 0;
                let mut decoded = Vec::new();
                let mut first = true;
                for call in 0..compressed.len() + expected.len() + 5 {
                    let end = if first { split } else { compressed.len() };
                    let operation = if first {
                        DecodeOperation::Process
                    } else {
                        DecodeOperation::Finish
                    };
                    let mut output = vec![0; if call % 3 == 0 { 0 } else { capacity }];
                    let progress = session
                        .process(&compressed[cursor..end], &mut output, operation)
                        .unwrap();
                    cursor += progress.consumed;
                    decoded.extend_from_slice(&output[..progress.produced]);
                    if cursor == split {
                        first = false;
                    }
                    if progress.status == DecoderStatus::Finished {
                        break;
                    }
                }
                assert!(session.is_finished(), "split {split}, capacity {capacity}");
                assert_eq!(decoded, expected);
                assert_eq!(cursor, compressed.len());
            }
        }
        for end in 0..compressed.len() {
            let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
            let mut output = vec![0; expected.len()];
            let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
            let progress = session
                .process(&compressed[..end], &mut output, DecodeOperation::Process)
                .unwrap();
            assert_eq!(progress.status, DecoderStatus::NeedsInput);
            assert!(matches!(
                session
                    .process(&[], &mut output, DecodeOperation::Finish)
                    .unwrap_err()
                    .error,
                mbrotli::DecodeError::UnexpectedEndOfInput
            ));
        }
    }
}

#[test]
fn aligned_fixture_headers_can_be_drained_without_affecting_members() {
    let mut wire = Wire::window(16, false);
    wire.raw_header(1);
    let mut bytes = wire.take();
    bytes.push(b'x');
    bytes.extend_from_slice(&wire.finish());
    assert_eq!(
        Decompressor::new(DecoderConfig::default())
            .unwrap()
            .decompress(&bytes)
            .unwrap(),
        b"x"
    );
}

#[cfg(feature = "experimental")]
#[test]
fn zero_bit_empty_custom_transform_is_rejected_instead_of_looping() {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let mut source = vec![0x91, 0, 0, 1, 1]; // signature, no prefix, one word list, two length-4 words
    source.extend_from_slice(&[0; 27]);
    source.extend_from_slice(b"abcdefgh");
    source.extend_from_slice(&[1, 1, 0, 0, 1, 0, 9, 0, 1, 0, 0, 0]); // one omit-last-9 transform
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Serialized(&source)],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    let mut wire = Wire::window(16, false);
    wire.bits(1, 0);
    wire.bits(2, 0);
    wire.bits(16, 0);
    wire.bits(1, 0); // one-byte compressed metablock
    wire.bits(3, 0);
    wire.bits(6, 4);
    wire.bits(2, 0);
    wire.bits(2, 0); // NDIRECT=1, no contexts
    for (width, symbol) in [(8, 0), (10, 130), (7, 16)] {
        wire.bits(2, 1);
        wire.bits(2, 0);
        wire.bits(width, symbol);
    }
    // All three trees have one symbol; insert=0, nominal copy=4,
    // transformed length=0, and the direct distance requires no extra bits.
    let compressed = wire.finish();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert!(matches!(
        decoder.decompress_with_dictionary(&dictionary, &compressed),
        Err(mbrotli::DecodeError::InvalidData {
            kind: mbrotli::InvalidDataKind::DictionaryReference
        })
    ));
}

#[test]
fn every_builtin_transform_is_reached_by_an_explicit_static_reference() {
    let triples = include_bytes!("../src/shared/dictionary/builtin_transforms.bin");
    let strings = include_bytes!("../src/shared/dictionary/builtin_prefix_suffix.bin");
    let words = include_bytes!("../src/shared/dictionary/words.bin");
    // RFC 7932: 32 words of length 24 occupy the dictionary's final 768 bytes.
    let word = &words[words.len() - 768..words.len() - 744];
    assert!(word.is_ascii());
    let stringlet = |id: u8| {
        let mut offset = 0;
        for _ in 0..id {
            offset += 1 + usize::from(strings[offset]);
        }
        &strings[offset + 1..offset + 1 + usize::from(strings[offset])]
    };
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    for (index, &[prefix, operation, suffix]) in triples.as_chunks::<3>().0.iter().enumerate() {
        let mut body = match operation {
            0..=9 => word[..word.len() - usize::from(operation)].to_vec(),
            12..=20 => word[usize::from(operation - 11)..].to_vec(),
            _ => word.to_vec(),
        };
        if operation == 10 {
            body[0].make_ascii_uppercase();
        }
        if operation == 11 {
            body.make_ascii_uppercase();
        }
        let expected = [stringlet(prefix), body.as_slice(), stringlet(suffix)].concat();
        let mut wire = Wire::window(22, false);
        wire.copy(24, (index * 32 + 1) as u64, expected.len());
        let compressed = wire.finish();
        assert_eq!(
            support::c_decompress(&compressed, expected.len()).unwrap(),
            expected,
            "C transform {index}"
        );
        assert_eq!(
            decoder.decompress(&compressed).unwrap(),
            expected,
            "Rust transform {index}"
        );
    }
}

#[test]
fn fifteen_raw_slots_preserve_order_and_static_dictionary_fallback() {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    for reverse in [false, true] {
        let mut prefixes: Vec<Vec<u8>> = (0..15)
            .map(|i| {
                if i % 3 == 0 {
                    Vec::new()
                } else {
                    vec![i as u8; 8]
                }
            })
            .collect();
        if reverse {
            prefixes.reverse();
        }
        let slices: Vec<&[u8]> = prefixes.iter().map(Vec::as_slice).collect();
        let attachments: Vec<_> = slices
            .iter()
            .map(|bytes| DictionaryAttachment::Raw(bytes))
            .collect();
        let dictionary =
            DecodeDictionary::new(&attachments, DecodeDictionaryLimits::default()).unwrap();
        let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
        let mut expected = Vec::new();
        let mut wire = Wire::window(10, false);
        let mut suffix_size: usize = slices.iter().map(|bytes| bytes.len()).sum();
        for bytes in &slices {
            if !bytes.is_empty() {
                wire.copy(
                    bytes.len(),
                    (expected.len() + suffix_size) as u64,
                    bytes.len(),
                );
                expected.extend_from_slice(bytes);
            }
            suffix_size -= bytes.len();
        }
        // First built-in length-four word after the complete RAW address space.
        let total_prefix: usize = slices.iter().map(|bytes| bytes.len()).sum();
        wire.copy(4, (expected.len() + total_prefix + 1) as u64, 4);
        expected.extend_from_slice(&include_bytes!("../src/shared/dictionary/words.bin")[..4]);
        let compressed = wire.finish();
        assert_eq!(
            support::c_decompress_with_prefixes(&slices, &compressed, expected.len()).unwrap(),
            expected
        );
        assert_eq!(
            decoder
                .decompress_with_dictionary(&dictionary, &compressed)
                .unwrap(),
            expected
        );
    }
}
