#![cfg(feature = "decompression")]
//! `decompress_to_slice` decodes a whole member straight into the caller's
//! slice, using it as history. A session given the same slice decodes through
//! the ring and delivers from it, so both must agree on every outcome: the
//! written length or the error, and on success or `OutputTooSmall` every byte
//! of the slice, including an untouched suffix.
mod support;

use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
use mbrotli::{
    DecodeError, DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus, Decompressor,
    MemberMode,
};

/// Marks slice bytes the decoder must not have written.
const SENTINEL: u8 = 0xa5;

/// The one-shot slice contract, spelled through the public ring-backed session.
fn through_session(
    decoder: &mut Decompressor,
    dictionary: Option<&DecodeDictionary>,
    src: &[u8],
    dst: &mut [u8],
) -> Result<usize, DecodeError> {
    let mut session = match dictionary {
        Some(dictionary) => {
            decoder.start_with_dictionary(dictionary, DecodeStreamConfig::default())
        }
        None => decoder.start(DecodeStreamConfig::default()),
    }?;
    let progress = session
        .process(src, dst, DecodeOperation::Finish)
        .map_err(|failure| failure.error)?;
    if progress.status == DecoderStatus::NeedsOutput {
        return Err(DecodeError::OutputTooSmall {
            written: progress.produced,
        });
    }
    if progress.consumed != src.len() {
        return Err(DecodeError::TrailingData {
            offset: progress.consumed as u64,
        });
    }
    Ok(progress.produced)
}

/// Compares both paths for every interesting destination length.
fn agree(
    label: &str,
    config: DecoderConfig,
    dictionary: Option<&DecodeDictionary>,
    src: &[u8],
    length: usize,
) {
    for (_, backend) in support::host_levels() {
        let build = || {
            Decompressor::builder(config)
                .with_backend(backend)
                .build()
                .unwrap()
        };
        let mut linear = build();
        let mut ring = build();
        let mut sizes = vec![
            0,
            1,
            length / 2,
            length.saturating_sub(1),
            length,
            length + 1,
            length + 37,
        ];
        sizes.dedup();
        for size in sizes {
            let mut direct = vec![SENTINEL; size];
            let mut delivered = vec![SENTINEL; size];
            let actual = match dictionary {
                Some(dictionary) => {
                    linear.decompress_with_dictionary_to_slice(dictionary, src, &mut direct)
                }
                None => linear.decompress_to_slice(src, &mut direct),
            };
            let expected = through_session(&mut ring, dictionary, src, &mut delivered);
            assert_eq!(
                format!("{actual:?}"),
                format!("{expected:?}"),
                "{label} backend {} size {size}",
                backend.name()
            );
            if matches!(actual, Ok(_) | Err(DecodeError::OutputTooSmall { .. })) {
                assert!(
                    direct == delivered,
                    "{label} backend {} size {size}",
                    backend.name()
                );
            }
        }
    }
}

#[test]
fn slices_match_ring_delivery_across_corpora_qualities_and_windows() {
    let mut corpora = support::structural_corpora();
    corpora.extend(support::vendor_corpora(1 << 18));
    for corpus in &corpora {
        for quality in [0, 1, 5, 9, 11] {
            // The optimal-parse qualities are slow; their shapes show on a prefix.
            let data = if quality >= 10 {
                &corpus.data[..corpus.data.len().min(1 << 16)]
            } else {
                &corpus.data[..]
            };
            for lgwin in [10, 16, 22] {
                let compressed = support::c_compress_native_one_shot(quality, lgwin, data);
                let label = format!("{} q{quality} w{lgwin}", corpus.name);
                agree(
                    &label,
                    DecoderConfig::default(),
                    None,
                    &compressed,
                    data.len(),
                );
            }
        }
    }
}

#[test]
fn damaged_streams_fail_the_same_way_in_slices_and_sessions() {
    let payload: Vec<u8> =
        include_bytes!("../brotli-ffi/vendor/brotli/tests/testdata/alice29.txt")[..20_000].to_vec();
    for quality in [1, 5, 11] {
        let compressed = support::c_compress_native_one_shot(quality, 16, &payload);
        for end in [1, compressed.len() / 3, compressed.len() - 1] {
            agree(
                "truncated",
                DecoderConfig::default(),
                None,
                &compressed[..end],
                payload.len(),
            );
        }
        for index in (0..compressed.len()).step_by(compressed.len() / 17 + 1) {
            let mut damaged = compressed.clone();
            damaged[index] ^= 0x10;
            agree(
                "damaged",
                DecoderConfig::default(),
                None,
                &damaged,
                payload.len(),
            );
        }
        let mut trailing = compressed.clone();
        trailing.push(0);
        agree(
            "trailing",
            DecoderConfig::default(),
            None,
            &trailing,
            payload.len(),
        );
    }
}

#[test]
fn later_members_and_dictionaries_match_ring_delivery() {
    let first = b"The first member repeats itself, repeats itself, repeats itself.".repeat(40);
    let second: Vec<u8> = (0..30_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let concatenated = [
        support::c_compress_native_one_shot(5, 16, &first),
        support::c_compress_native_one_shot(9, 18, &second),
    ]
    .concat();
    agree(
        "concatenated",
        DecoderConfig::default().with_member_mode(MemberMode::Concatenated),
        None,
        &concatenated,
        first.len() + second.len(),
    );

    let prefix = b"Content-Type: application/json\r\nX-Request-ID: 0123456789abcdef\r\n".repeat(8);
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Raw(&prefix)],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    let payload = [&prefix[..], b"tail after the prefix", &prefix[..40]].concat();
    for quality in [2, 5, 11] {
        for lgwin in [10, 22] {
            let compressed = support::c_compress_with_prefixes(
                support::CParams::new(quality, lgwin),
                &[&prefix],
                &payload,
            );
            agree(
                "dictionary",
                DecoderConfig::default(),
                Some(&dictionary),
                &compressed,
                payload.len(),
            );
        }
    }
}
