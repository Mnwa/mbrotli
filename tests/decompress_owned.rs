#![cfg(feature = "decompression")]
mod support;

use mbrotli::{DecodeError, DecodeLimits, DecoderConfig, Decompressor, MemberMode};

#[test]
fn owned_history_matches_slice_decoding_before_at_and_after_window_wrap() {
    for (_, backend) in support::host_levels() {
        for quality in [0, 5, 11] {
            for length in [0, 1, 63, 64, 65, 1023, 1024, 1025, 4097, 16384] {
                for payload in [
                    vec![b'x'; length],
                    (0..length).map(|i| (i * 173 % 251) as u8).collect(),
                ] {
                    let compressed = support::c_compress_native_one_shot(quality, 10, &payload);
                    let mut decoder = Decompressor::builder(DecoderConfig::default())
                        .with_backend(backend)
                        .build()
                        .unwrap();
                    let owned = decoder.decompress(&compressed).unwrap();
                    assert_eq!(owned, payload);
                    let mut slice = vec![0; length];
                    decoder
                        .decompress_to_slice(&compressed, &mut slice)
                        .unwrap();
                    assert_eq!(slice, owned);
                    assert_eq!(decoder.decompress(&compressed).unwrap(), owned);
                    // Reuse must not mutate the transferred allocation.
                    assert_eq!(owned, payload);
                }
            }
        }
    }
}

#[test]
fn collected_output_keeps_limits_truncation_and_append_rollback() {
    for quality in [0, 5, 11] {
        let payload = vec![b'z'; 4097];
        let compressed = support::c_compress_native_one_shot(quality, 22, &payload);
        for limit in [0, 1, 4096, 4097, 4098] {
            let config = DecoderConfig::default()
                .with_limits(DecodeLimits::default().with_max_output_bytes(Some(limit)));
            let result = Decompressor::new(config).unwrap().decompress(&compressed);
            if limit < payload.len() as u64 {
                assert!(matches!(
                    result,
                    Err(DecodeError::OutputLimitExceeded { .. })
                ));
            } else {
                assert_eq!(result.unwrap(), payload);
            }
        }
        for end in 0..compressed.len() {
            let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
            let mut output = Vec::new();
            assert!(
                decoder
                    .decompress_into(&compressed[..end], &mut output)
                    .is_err()
            );
            assert!(output.is_empty());
        }
        let mut tailed = compressed.clone();
        tailed.push(0);
        let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
        assert!(matches!(
            decoder.decompress(&tailed),
            Err(DecodeError::TrailingData { .. })
        ));
        let mut prefix = b"prefix".to_vec();
        assert!(decoder.decompress_into(&tailed, &mut prefix).is_err());
        assert_eq!(prefix, b"prefix");
    }
}

#[test]
fn concatenated_owned_members_keep_separate_histories() {
    let a = vec![b'a'; 4097];
    let b = vec![b'b'; 1025];
    let mut compressed = support::c_compress_native_one_shot(5, 10, &a);
    compressed.extend(support::c_compress_native_one_shot(0, 10, &b));
    let config = DecoderConfig::default().with_member_mode(MemberMode::Concatenated);
    assert_eq!(
        Decompressor::new(config)
            .unwrap()
            .decompress(&compressed)
            .unwrap(),
        [a, b].concat()
    );
}

#[test]
fn stored_short_headers_match_streaming_for_every_bit_mutation() {
    for window in 18..=24 {
        for length in [1usize, 2, 255, 256, 65535, 65536] {
            let header = 0x80_0001u32 | ((window - 17) << 1) | ((length as u32 - 1) << 7);
            let mut input = header.to_le_bytes()[..3].to_vec();
            input.extend((0..length).map(|i| i as u8));
            input.push(3);
            let expected = input[3..3 + length].to_vec();
            let config = DecoderConfig::default();
            assert_eq!(
                Decompressor::new(config)
                    .unwrap()
                    .decompress(&input)
                    .unwrap(),
                expected
            );
            let mut output = vec![0; length];
            Decompressor::new(config)
                .unwrap()
                .decompress_to_slice(&input, &mut output)
                .unwrap();
            assert_eq!(output, expected);
            if length == 255 {
                for bit in 0..32 {
                    let byte = if bit < 24 { bit / 8 } else { input.len() - 1 };
                    input[byte] ^= 1 << (bit % 8);
                    let owned = Decompressor::new(config).unwrap().decompress(&input);
                    // Appending into a nonempty vector exercises the full driver.
                    let mut baseline = vec![42];
                    let result = Decompressor::new(config)
                        .unwrap()
                        .decompress_into(&input, &mut baseline);
                    match (owned, result) {
                        (Ok(bytes), Ok(range)) => assert_eq!(bytes, baseline[range]),
                        (Err(a), Err(b)) => {
                            assert_eq!(std::mem::discriminant(&a), std::mem::discriminant(&b));
                            assert_eq!(a.to_string(), b.to_string());
                        }
                        pair => panic!("stored/driver mismatch: {pair:?}"),
                    }
                    input[byte] ^= 1 << (bit % 8);
                }
            }
        }
    }
}
