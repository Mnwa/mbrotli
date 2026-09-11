#![cfg(all(feature = "decompression", feature = "experimental"))]
use mbrotli::framing::*;
use mbrotli::{DecodeOperation, OutputSize, RetentionPolicy};

fn var(mut n: u64) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        let low = (n & 127) as u8;
        n >>= 7;
        b.push(low | if n == 0 { 0 } else { 128 });
        if n == 0 {
            return b;
        }
    }
}
fn chunk(header: &[u8], content: &[u8]) -> Vec<u8> {
    let mut b = var((header.len() + content.len()) as u64);
    b.extend_from_slice(header);
    b.extend_from_slice(content);
    b
}
fn full(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0x91, 10, 66, 82, 4];
    for c in chunks {
        b.extend(c);
    }
    b.extend(chunk(&[10], &[0, 0]));
    b
}
fn single(payload: &[u8]) -> Vec<u8> {
    let mut b = vec![0x91, 10, 66, 82, 0];
    b.extend(chunk(&[2, 0, 0], payload));
    b
}
fn auto() -> FramedDecompressor {
    FramedDecompressor::new(FramedDecodeConfig::default().with_input_mode(InputMode::Auto)).unwrap()
}
fn decoder() -> FramedDecompressor {
    FramedDecompressor::new(Default::default()).unwrap()
}

#[test]
fn detection_distinguishes_raw_dictionary_and_invalid_signatures() {
    assert!(matches!(
        decoder().decompress(&[0x3b]),
        Err(FramedDecodeError::InvalidSignature)
    ));
    let o = auto().decompress(&[0x3b]).unwrap();
    assert!(matches!(o.structure, OutputStructure::Raw));
    assert_eq!(o.resources.len(), 1);
    assert!(o.resources[0].data.is_empty());
    assert_eq!(o.resources[0].source, ResourceSource::Raw);
    for bytes in [&[][..], &[0x91], &[0x91, 10], &[0x91, 10, 66]] {
        assert!(
            matches!(
                auto().decompress(bytes),
                Err(FramedDecodeError::UnexpectedEndOfInput)
            ),
            "{bytes:?}"
        );
    }
    assert!(matches!(
        auto().decompress(&[0x91, 0]),
        Err(FramedDecodeError::UnexpectedInputKind(
            UnexpectedInputKind::SerializedDictionary
        ))
    ));
    assert!(matches!(
        auto().decompress(&[0x91, 11]),
        Err(FramedDecodeError::InvalidSignature)
    ));
    assert!(matches!(
        auto().decompress(&[0x91, 10, 66, 82, 1]),
        Err(FramedDecodeError::UnsupportedVersion(1))
    ));
    assert!(matches!(
        auto().decompress(&[0x3b, 0]),
        Err(FramedDecodeError::TrailingData)
    ));
}
#[test]
fn section_8_1_set_bit_requires_footer_and_permits_empty_full_profile() {
    let bytes = full(&[]);
    let o = decoder().decompress(&bytes).unwrap();
    assert!(o.resources.is_empty());
    assert!(
        matches!(o.structure, OutputStructure::Framed { header, layout } if header.has_footer() && layout.footer.unwrap().file_size.is_none())
    );
    assert!(decoder().decompress(&bytes[..5]).is_err());
}
#[test]
fn section_8_1_clear_bit_needs_one_resource_and_accepts_terminal_padding() {
    let mut bytes = single(b"hello");
    bytes.extend([0, 3, 0, 0, 0]);
    assert_eq!(
        decoder().decompress(&bytes).unwrap().resources[0].data,
        b"hello"
    );
    bytes.extend(chunk(&[2, 0, 0], b"second"));
    assert!(matches!(
        decoder().decompress(&bytes),
        Err(FramedDecodeError::InvalidOrder)
    ));
    assert!(decoder().decompress(&[0x91, 10, 66, 82, 0]).is_err());
}
#[test]
fn independent_partial_hidden_metadata_and_padding_preserve_structure() {
    let bytes = full(&[
        chunk(&[7, 0], b"AA\x02\xff\x00AA\x00"),
        chunk(&[1, 0], b"id\x00mt\x08\x01\0\0\0\0\0\0\0"),
        chunk(&[3, 0, 1], b"first"),
        vec![0],
        chunk(&[4, 0, 0], b"middle"),
        chunk(&[5, 0, 0], b"last"),
        chunk(&[6, 0], b"ZZ\x01!"),
        chunk(&[2, 0, 0], b""),
    ]);
    let out = decoder().decompress(&bytes).unwrap();
    assert_eq!(out.resources.len(), 2);
    assert_eq!(out.resources[0].data, b"firstmiddlelast");
    assert!(out.resources[0].hidden);
    let m = out.resources[0].metadata.as_ref().unwrap();
    assert_eq!(m.name(), Some(""));
    assert_eq!(m.timestamp(), Some(1));
    assert_eq!(m.fields().count(), 2);
    assert_eq!(out.global_metadata[0].fields().count(), 2);
    assert_eq!(
        out.resources[0]
            .footer_metadata
            .as_ref()
            .unwrap()
            .fields()
            .next()
            .unwrap()
            .value,
        b"!"
    );
}
fn collect_incremental(bytes: &[u8], split: usize, capacity: usize) -> Vec<u8> {
    let mut d = auto();
    let mut session = d.start(Default::default()).unwrap();
    let mut p = 0;
    let mut final_phase = false;
    let mut payload = Vec::new();
    let mut previous_offset = 0;
    let mut last_resource = ResourceIndex(0);
    for step in 0..10000 {
        if p == split {
            final_phase = true;
        }
        let end = if final_phase { bytes.len() } else { split };
        let operation = if final_phase {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let mut output = vec![0; if step % 7 == 0 { 0 } else { capacity }];
        let result = session
            .process(&bytes[p..end], &mut output, operation)
            .unwrap();
        assert!(result.consumed <= end - p);
        assert!(result.produced <= if step % 7 == 0 { 0 } else { capacity });
        p += result.consumed;
        match result.status {
            FramedDecoderStatus::Event(FramedEvent::ResourceData(data)) => {
                assert_eq!(data.bytes.len(), result.produced);
                if data.position.resource != last_resource {
                    previous_offset = 0;
                    last_resource = data.position.resource;
                }
                assert_eq!(data.position.offset, previous_offset);
                previous_offset += data.bytes.len() as u64;
                payload.extend_from_slice(data.bytes);
            }
            FramedDecoderStatus::Finished => {
                assert_eq!((result.consumed, result.produced), (0, 0));
                assert_eq!(p, bytes.len());
                assert_eq!(session.total_out(), payload.len() as u64);
                return payload;
            }
            _ => assert_eq!(result.produced, 0),
        }
    }
    panic!("zero progress loop")
}
#[test]
fn every_input_split_and_small_output_matches_owned_results() {
    let raw = [0x0b, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03];
    let compressed = full(&[chunk(&[2, 2, 5, 0], &raw)]);
    for bytes in [
        raw.to_vec(),
        single(b"hello"),
        compressed,
        full(&[chunk(&[2, 0, 0], b"hello"), chunk(&[2, 0, 0], b"again")]),
    ] {
        let expected: Vec<_> = auto()
            .decompress(&bytes)
            .unwrap()
            .resources
            .into_iter()
            .flat_map(|r| r.data)
            .collect();
        for split in 0..=bytes.len() {
            for capacity in [1, 2, 7, 31, 32, 63, 64, 4096] {
                assert_eq!(
                    collect_incremental(&bytes, split, capacity),
                    expected,
                    "split {split} output {capacity}"
                );
            }
        }
    }
}
#[test]
fn nonminimal_varints_are_accepted_and_original_headers_match_directory() {
    let mut bytes = vec![0x91, 10, 66, 82, 4];
    let header = [0x84, 0, 2, 0, 0];
    bytes.extend(header);
    bytes.push(b'x');
    let offset = bytes.len() as u64;
    let mut directory = vec![0, 5, header.len() as u8];
    directory.extend(header);
    bytes.extend(chunk(&[9], &directory));
    let mut pointer = var(offset);
    pointer.reverse();
    let mut footer = vec![0];
    footer.extend(pointer);
    bytes.extend(chunk(&[10], &footer));
    let out = decoder().decompress(&bytes).unwrap();
    assert_eq!(out.resources[0].data, b"x");
    let OutputStructure::Framed { layout, .. } = out.structure else {
        panic!()
    };
    assert_eq!(layout.chunks[0].header_bytes, header);
    assert_eq!(layout.directory.unwrap().entries.len(), 1);
    bytes[16] ^= 1;
    assert!(decoder().decompress(&bytes).is_err());
}
#[test]
fn rollback_and_slice_failure_preserve_exact_last_resource_fragment() {
    let bytes = full(&[chunk(&[2, 0, 0], b"abc"), chunk(&[2, 0, 0], b"xyz")]);
    let mut d = decoder();
    let mut appended = b"prefix".to_vec();
    let output = d.decompress_into(&bytes, &mut appended).unwrap();
    assert_eq!(output.resources[0].data, 6..9);
    assert_eq!(output.resources[1].data, 9..12);
    let mut bad = bytes.clone();
    bad.push(0);
    let prefix = appended.clone();
    assert!(d.decompress_into(&bad, &mut appended).is_err());
    assert_eq!(appended, prefix);
    let mut slice = [b'!'; 5];
    let error = d.decompress_to_slice(&bytes, &mut slice).unwrap_err();
    assert!(matches!(error.error, FramedDecodeError::OutputTooSmall));
    assert_eq!(error.produced, 5);
    assert_eq!(&slice, b"abcxy");
    let last = error.last_output.unwrap();
    assert_eq!(last.range, 3..5);
    assert_eq!(last.position.resource, ResourceIndex(1));
    assert_eq!(last.position.offset, 0);
    let mut slice = [b'!'; 8];
    let o = d.decompress_to_slice(&bytes, &mut slice).unwrap();
    assert_eq!(o.resources[1].data, 3..6);
    assert_eq!(&slice[6..], b"!!");
}
#[test]
fn finish_boundary_failed_state_and_abandoned_protection() {
    let mut d = auto();
    {
        let mut s = d.start(Default::default()).unwrap();
        assert!(matches!(
            s.process(&[], &mut [], DecodeOperation::Process)
                .unwrap()
                .status,
            FramedDecoderStatus::NeedsInput
        ));
        s.process(&[0x3b], &mut [], DecodeOperation::Finish)
            .unwrap();
        assert!(matches!(
            s.process(&[], &mut [], DecodeOperation::Finish)
                .unwrap_err()
                .error,
            FramedDecodeError::InvalidState
        ));
        assert!(matches!(
            s.process(&[0x3b], &mut [], DecodeOperation::Finish)
                .unwrap_err()
                .error,
            FramedDecodeError::InvalidState
        ));
    }
    core::mem::forget(d.start(Default::default()).unwrap());
    d.trim(RetentionPolicy::ReleaseAll);
    assert!(matches!(
        d.start(Default::default()),
        Err(FramedDecodeError::AbandonedSession)
    ));
    d.recover();
    assert!(d.decompress(&[0x3b]).is_ok());
    let mut fork = d.fork_empty();
    assert_eq!(fork.config(), d.config());
    assert_eq!(fork.retention(), d.retention());
    assert_eq!(fork.retained_bytes(), 0);
    assert!(fork.decompress(&[0x3b]).is_ok());
    core::mem::forget(d.start(Default::default()).unwrap());
    d.reconfigure(*d.config()).unwrap();
    assert!(d.decompress(&[0x3b]).is_ok());
}
#[test]
fn limits_and_exact_size_apply_across_resources() {
    let bytes = full(&[chunk(&[2, 0, 0], b"abc"), chunk(&[2, 0, 0], b"xyz")]);
    for maximum in [0, 5, 6, 7] {
        let config = FramedDecodeConfig::default()
            .with_limits(FramedDecodeLimits::default().with_max_output_bytes(Some(maximum)));
        let result = FramedDecompressor::new(config).unwrap().decompress(&bytes);
        assert_eq!(result.is_ok(), maximum >= 6);
    }
    let mut d = decoder();
    let mut s = d.start(OutputSize::Exact(5).into()).unwrap();
    let mut p = 0;
    loop {
        let mut out = [0; 9];
        match s.process(&bytes[p..], &mut out, DecodeOperation::Finish) {
            Ok(progress) => p += progress.consumed,
            Err(_) => break,
        }
    }
}
#[test]
fn metadata_validation_rejects_reserved_duplicates_and_broken_lengths() {
    for content in [
        &b"id\x01aid\x01b"[..],
        b"xx\x00",
        b"iD\x00",
        b"id\x01\xff",
        b"mt\x00",
        b"AA\x02x",
    ] {
        assert!(
            decoder()
                .decompress(&full(&[chunk(&[1, 0], content), chunk(&[2, 0, 0], b"")]))
                .is_err()
        );
    }
    assert!(decoder().decompress(&full(&[chunk(&[1, 0], b"")])).is_err());
    assert!(decoder().decompress(&full(&[chunk(&[6, 0], b"")])).is_err());
}
#[cfg(all(feature = "compression", not(feature = "no_std")))]
#[test]
fn existing_writer_compressed_partials_metadata_and_repeats_decode() {
    use std::io::Write;
    let mut encoder = mbrotli::Compressor::new(Default::default()).unwrap();
    let mut writer = encoder
        .framed_writer(
            Vec::new(),
            FramingConfig {
                chunk_bytes: 17,
                repeat_metadata: true,
                ..Default::default()
            },
        )
        .unwrap();
    writer
        .metadata(
            MetadataKind::Resource,
            &[MetadataField {
                code: *b"id",
                value: b"name",
            }],
        )
        .unwrap();
    {
        let mut r = writer
            .resource(Default::default(), Default::default())
            .unwrap();
        r.write_all(&b"some resource data ".repeat(20)).unwrap();
        r.try_finish().unwrap();
    }
    let bytes = writer.finish().unwrap();
    let output = decoder().decompress(&bytes).unwrap();
    assert_eq!(output.resources[0].data, b"some resource data ".repeat(20));
}
#[cfg(not(feature = "no_std"))]
#[test]
fn reader_preserves_suffix_and_delivers_empty_resource_events() {
    use std::io::{BufRead, BufReader, Cursor};
    let mut bytes = full(&[chunk(&[2, 0, 0], b"abc")]);
    bytes.extend(b"TAIL");
    let mut d = decoder();
    let mut reader = d
        .framed_reader(BufReader::new(Cursor::new(bytes)), Default::default())
        .unwrap();
    let mut payload = Vec::new();
    while let Some(event) = reader.next_event().unwrap() {
        if let FramedEvent::ResourceData(data) = event {
            payload.extend_from_slice(data.bytes);
        }
    }
    assert_eq!(payload, b"abc");
    assert!(reader.is_finished());
    let mut source = reader.into_inner();
    assert_eq!(source.fill_buf().unwrap(), b"TAIL");
}

#[path = "decode_support/wire.rs"]
mod raw_wire;
fn stored(payload: &[u8]) -> Vec<u8> {
    let mut w = raw_wire::Wire::window(22, false);
    w.raw(payload);
    w.finish()
}
#[test]
fn codec_continues_from_metadata_to_resource_across_padding() {
    let raw = stored(b"id\x01xhello");
    let bytes = full(&[
        chunk(&[1, 2, 4], &raw[..7]),
        vec![0],
        chunk(&[2, 1, 5, 0], &raw[7..]),
    ]);
    let output = decoder().decompress(&bytes).unwrap();
    assert_eq!(
        output.resources[0].metadata.as_ref().unwrap().name(),
        Some("x")
    );
    assert_eq!(output.resources[0].data, b"hello");
    for split in 0..=bytes.len() {
        assert_eq!(collect_incremental(&bytes, split, 1), b"hello");
    }
}
#[test]
fn declared_content_size_and_codec_transitions_are_validated() {
    let raw = stored(b"hello");
    for size in [0, 4, 6] {
        assert!(
            decoder()
                .decompress(&full(&[chunk(&[2, 2, size, 0], &raw)]))
                .is_err()
        );
    }
    let mut tail = raw.clone();
    tail.push(0);
    assert!(
        decoder()
            .decompress(&full(&[chunk(&[2, 2, 5, 0], &tail)]))
            .is_err()
    );
    assert!(
        decoder()
            .decompress(&full(&[chunk(&[2, 1, 0, 0], b"")]))
            .is_err()
    );
    let bytes = full(&[chunk(&[3, 2, 2, 0], &raw[..5]), chunk(&[5, 0, 0], b"llo")]);
    assert!(decoder().decompress(&bytes).is_err());
    let bytes = full(&[chunk(&[2, 2, 5, 0], &raw), chunk(&[2, 1, 0, 0], b"")]);
    assert!(decoder().decompress(&bytes).is_err());
    let mut large = raw_wire::Wire::window(22, true);
    large.raw(b"x");
    let large = large.finish();
    assert!(
        decoder()
            .decompress(&full(&[chunk(&[2, 2, 1, 0], &large)]))
            .is_err()
    );
    assert_eq!(
        decoder()
            .decompress(&full(&[chunk(&[2, 3, 1, 0, 0], &large)]))
            .unwrap()
            .resources[0]
            .data,
        b"x"
    );
}
struct Resolver {
    calls: std::cell::Cell<usize>,
    bytes: Vec<u8>,
}
impl DictionaryResolver for Resolver {
    fn resolve(&self, request: ExternalDictionaryRequest) -> Option<&[u8]> {
        self.calls.set(self.calls.get() + 1);
        (request.id == DictionaryId([7; 32])).then_some(&self.bytes)
    }
}
fn external_header(flags: u8) -> Vec<u8> {
    let mut h = vec![2, 3, 0, 1, flags, 3];
    h.extend([7; 32]);
    h.push(0);
    h
}
#[test]
fn external_dictionary_resolution_is_explicit_once_and_source_preserving() {
    let resolver = Resolver {
        calls: std::cell::Cell::new(0),
        bytes: vec![0x91, 0],
    };
    let bytes = full(&[chunk(&external_header(2), &[0x3b])]);
    let mut d = decoder();
    let lookup = DictionaryResolverRef::from(&resolver);
    assert_eq!(format!("{lookup:?}"), "DictionaryResolverRef { .. }");
    assert!(d.decompress_with_dictionaries(lookup, &bytes).is_ok());
    assert_eq!(resolver.calls.get(), 1);
    let mut dst = b"prefix".to_vec();
    d.decompress_with_dictionaries_into(&resolver, &bytes, &mut dst)
        .unwrap();
    assert_eq!(dst, b"prefix");
    d.decompress_with_dictionaries_to_slice(&resolver, &bytes, &mut [])
        .unwrap();
    let bad = full(&[chunk(&external_header(6), &[0x3b])]);
    let error = d.decompress_with_dictionaries(&resolver, &bad).unwrap_err();
    assert!(std::error::Error::source(&error).is_some());
    assert!(matches!(error, FramedDecodeError::Dictionary { .. }));
    let error = d.decompress(&bytes).unwrap_err();
    assert!(matches!(error, FramedDecodeError::MissingDictionary { .. }));
    if let FramedDecodeError::MissingDictionary { location, .. } = error {
        assert_eq!(location.chunk(), Some(ChunkOffset(5)));
        assert_eq!(location.resource(), Some(ResourceIndex(0)));
    }
    let before = resolver.calls.get();
    auto()
        .decompress_with_dictionaries(&resolver, &[0x3b])
        .unwrap();
    assert_eq!(resolver.calls.get(), before);
}
#[test]
fn internal_references_cover_full_partial_and_metadata_contents() {
    for (prefix, flags, target) in [
        (vec![chunk(&[2, 0, 1], b"prefix")], 0, 5),
        (
            vec![chunk(&[3, 0, 1], b"pre"), chunk(&[5, 0, 0], b"fix")],
            0,
            5,
        ),
        (
            vec![chunk(&[3, 0, 1], b"pre"), chunk(&[5, 0, 0], b"fix")],
            1,
            5,
        ),
        (vec![chunk(&[7, 0], b"AA\x00")], 1, 5),
        (vec![chunk(&[2, 0, 1], &[0x91, 0, 0, 0, 0])], 4, 5),
    ] {
        let mut chunks = prefix;
        chunks.push(chunk(&[2, 3, 0, 1, flags, target, 0], &[0x3b]));
        let bytes = full(&chunks);
        assert!(decoder().decompress(&bytes).is_ok());
        let config = FramedDecodeConfig::default()
            .with_internal_dictionaries(InternalDictionaryPolicy::Reject);
        assert!(matches!(
            FramedDecompressor::new(config).unwrap().decompress(&bytes),
            Err(FramedDecodeError::InternalDictionaryReferencesDisabled)
        ));
    }
    for pointer in [0, 6, 255] {
        let bytes = full(&[
            chunk(&[2, 0, 0], b"prefix"),
            chunk(&[2, 3, 0, 1, 0, pointer, 0], &[0x3b]),
        ]);
        assert!(decoder().decompress(&bytes).is_err());
    }
}
#[test]
fn reject_policy_streams_without_dictionary_cache_and_retain_enforces_budget() {
    let bytes = single(&vec![b'x'; 10000]);
    let limits = FramedDecodeLimits::default().with_max_dictionary_bytes(Some(0));
    let retain = FramedDecodeConfig::default().with_limits(limits);
    assert!(matches!(
        FramedDecompressor::new(retain).unwrap().decompress(&bytes),
        Err(FramedDecodeError::LimitExceeded {
            kind: FramedLimitKind::DictionaryBytes,
            ..
        })
    ));
    let reject = retain.with_internal_dictionaries(InternalDictionaryPolicy::Reject);
    assert_eq!(
        FramedDecompressor::new(reject)
            .unwrap()
            .decompress(&bytes)
            .unwrap()
            .resources[0]
            .data
            .len(),
        10000
    );
}
#[test]
fn every_truncated_prefix_fails_without_infinite_input_waits() {
    let bytes = full(&[
        chunk(&[1, 0], b"id\x01x"),
        chunk(&[2, 2, 5, 0], &stored(b"hello")),
    ]);
    for length in 0..bytes.len() {
        assert!(
            decoder().decompress(&bytes[..length]).is_err(),
            "prefix {length}"
        );
    }
}

#[test]
fn numeric_limits_distinguish_zero_exact_boundary_and_unlimited() {
    let bytes = full(&[
        chunk(&[7, 0], b"AA\x00"),
        chunk(&[2, 0, 0], b"abc"),
        chunk(&[2, 0, 0], b"xyz"),
    ]);
    type LimitSetter = fn(FramedDecodeLimits, Option<u64>) -> FramedDecodeLimits;
    let cases: &[(LimitSetter, u64)] = &[
        (FramedDecodeLimits::with_max_input_bytes, bytes.len() as u64),
        (FramedDecodeLimits::with_max_output_bytes, 6),
        (FramedDecodeLimits::with_max_decoded_bytes, 9),
        (FramedDecodeLimits::with_max_resource_bytes, 3),
        (FramedDecodeLimits::with_max_metadata_bytes, 3),
        (FramedDecodeLimits::with_max_metadata_fields, 1),
        (FramedDecodeLimits::with_max_resources, 2),
        (FramedDecodeLimits::with_max_chunks, 4),
    ];
    for &(set, threshold) in cases {
        for limit in [
            None,
            Some(0),
            Some(threshold - 1),
            Some(threshold),
            Some(threshold + 1),
        ] {
            let config = FramedDecodeConfig::default().with_limits(set(Default::default(), limit));
            let result = FramedDecompressor::new(config).unwrap().decompress(&bytes);
            assert_eq!(
                result.is_ok(),
                limit.is_none_or(|n| n >= threshold),
                "threshold {threshold}, limit {limit:?}: {result:?}"
            );
        }
    }
    type HeapSetter = fn(FramedDecodeLimits, Option<usize>) -> FramedDecodeLimits;
    for set in [
        FramedDecodeLimits::with_max_workspace_bytes as HeapSetter,
        FramedDecodeLimits::with_max_framing_bytes,
        FramedDecodeLimits::with_max_dictionary_bytes,
    ] {
        let succeeds = |limit| {
            FramedDecompressor::new(
                FramedDecodeConfig::default().with_limits(set(Default::default(), limit)),
            )
            .unwrap()
            .decompress(&bytes)
            .is_ok()
        };
        assert!(succeeds(None));
        assert!(!succeeds(Some(0)));
        let mut low = 0;
        let mut high = 1 << 20;
        while low + 1 < high {
            let mid = (low + high) / 2;
            if succeeds(Some(mid)) {
                high = mid;
            } else {
                low = mid;
            }
        }
        assert!(!succeeds(Some(high - 1)));
        assert!(succeeds(Some(high)));
        assert!(succeeds(Some(high + 1)));
    }
}
#[test]
fn configuration_accessors_and_retention_preserve_independent_owner_policy() {
    let l = FramedDecodeLimits::default();
    assert_eq!(l.max_input_bytes(), None);
    assert_eq!(l.max_output_bytes(), None);
    assert_eq!(l.max_decoded_bytes(), None);
    assert_eq!(l.max_resource_bytes(), None);
    assert_eq!(l.max_workspace_bytes(), Some(64 << 20));
    assert_eq!(l.max_framing_bytes(), Some(8 << 20));
    assert_eq!(l.max_dictionary_bytes(), Some(32 << 20));
    assert_eq!(l.max_metadata_bytes(), Some(1 << 20));
    assert_eq!(l.max_metadata_fields(), Some(65536));
    assert_eq!(l.max_resources(), Some(10000));
    assert_eq!(l.max_chunks(), Some(1000000));
    for retention in [
        RetentionPolicy::Aggressive,
        RetentionPolicy::CurrentConfig,
        RetentionPolicy::ReleaseAll,
        RetentionPolicy::Bounded { max_bytes: 0 },
    ] {
        for backend in mbrotli::Backend::available() {
            let config = FramedDecodeConfig::default()
                .with_input_mode(InputMode::Auto)
                .with_window_limit(mbrotli::WindowLimit::standard(22).unwrap());
            let mut d = FramedDecompressor::builder(config)
                .with_backend(backend)
                .with_retention(retention)
                .build()
                .unwrap();
            let resolver = Resolver {
                calls: std::cell::Cell::new(0),
                bytes: Vec::new(),
            };
            {
                let mut s = d
                    .start_with_dictionaries(
                        &resolver,
                        FramedDecodeStreamConfig::default().with_output_size(OutputSize::Exact(0)),
                    )
                    .unwrap();
                assert_eq!(s.input_format(), None);
                assert_eq!(s.total_decoded(), 0);
                assert_eq!(s.resources_decoded(), 0);
                assert!(format!("{s:?}").contains("FramedDecoderSession"));
                let mut p = 0;
                loop {
                    let progress = s
                        .process(&[0x3b][p..], &mut [], DecodeOperation::Finish)
                        .unwrap();
                    p += progress.consumed;
                    if matches!(progress.status, FramedDecoderStatus::Finished) {
                        break;
                    }
                }
                assert_eq!(s.total_in(), 1);
                assert_eq!(s.resources_decoded(), 1);
                assert_eq!(s.input_format(), Some(StreamInfo::Raw));
                assert!(matches!(
                    s.process(&[], &mut [], DecodeOperation::Process)
                        .unwrap()
                        .status,
                    FramedDecoderStatus::Finished
                ));
            }
            assert_eq!(d.retention(), retention);
            d.reconfigure(config.with_window_limit(mbrotli::WindowLimit::standard(24).unwrap()))
                .unwrap();
        }
    }
    let f = auto().decompress_to_slice(&[], &mut []).unwrap_err();
    assert!(matches!(
        f.into_error(),
        FramedDecodeError::UnexpectedEndOfInput
    ));
    let location = FramedDecodeLocation::default();
    assert_eq!(location.chunk(), None);
    assert_eq!(location.resource(), None);
}

#[cfg(not(feature = "no_std"))]
#[test]
fn reader_retries_interrupted_preserves_wouldblock_and_exposes_failed_payload() {
    use std::io::{self, BufRead, Read};
    #[derive(Debug)]
    struct Faults {
        bytes: Vec<u8>,
        cursor: usize,
        calls: usize,
    }
    impl Read for Faults {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let src = self.fill_buf()?;
            let n = src.len().min(b.len());
            b[..n].copy_from_slice(&src[..n]);
            self.consume(n);
            Ok(n)
        }
    }
    impl BufRead for Faults {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            self.calls += 1;
            match self.calls {
                1 => Err(io::ErrorKind::Interrupted.into()),
                2 => Err(io::ErrorKind::WouldBlock.into()),
                _ => Ok(&self.bytes[self.cursor..]),
            }
        }
        fn consume(&mut self, n: usize) {
            self.cursor += n;
        }
    }
    let mut raw = stored(b"hello");
    *raw.last_mut().unwrap() = 0xff;
    let resolver = Resolver {
        calls: std::cell::Cell::new(0),
        bytes: Vec::new(),
    };
    let mut d = auto();
    let mut reader = d
        .framed_reader_with_dictionaries(
            &resolver,
            Faults {
                bytes: raw,
                cursor: 0,
                calls: 0,
            },
            Default::default(),
        )
        .unwrap();
    assert!(
        matches!(reader.next_event(),Err(FramedReadError::Io(e)) if e.kind()==io::ErrorKind::WouldBlock)
    );
    assert!(reader.partial_output().is_none());
    assert_eq!(reader.get_ref().cursor, 0);
    assert_eq!(reader.get_mut().calls, 2);
    assert!(format!("{reader:?}").contains("FramedReader"));
    loop {
        if let Err(FramedReadError::Decode(failure)) = reader.next_event() {
            assert_eq!(failure.produced, 5);
            let partial = reader.partial_output().unwrap();
            assert_eq!(partial.bytes, b"hello");
            assert_eq!(partial.position.resource, ResourceIndex(0));
            break;
        }
    }
    assert!(
        matches!(reader.next_event(),Err(FramedReadError::Decode(f)) if matches!(f.error,FramedDecodeError::InvalidState))
    );
}

fn directory_fixture(chunks: &[(Vec<u8>, Vec<u8>)], repeat: Option<usize>) -> Vec<u8> {
    let mut bytes = vec![0x91, 10, 66, 82, 4];
    let mut entries = Vec::new();
    let mut repeat_offset = 0;
    for (i, (header, payload)) in chunks.iter().enumerate() {
        let offset = bytes.len();
        if repeat == Some(i) {
            repeat_offset = offset;
        }
        let mut wire_header = var((header.len() + payload.len()) as u64);
        wire_header.extend(header);
        entries.push((offset, wire_header.clone()));
        bytes.extend(wire_header);
        bytes.extend(payload);
    }
    let offset = bytes.len();
    let mut directory = var(repeat_offset as u64);
    for (pointer, header) in entries {
        directory.extend(var(pointer as u64));
        directory.extend(var(header.len() as u64));
        directory.extend(header);
    }
    bytes.extend(chunk(&[9], &directory));
    let mut pointer = var(offset as u64);
    pointer.reverse();
    let mut footer = vec![0];
    footer.extend(pointer);
    bytes.extend(chunk(&[10], &footer));
    bytes
}
#[test]
fn independent_repeated_metadata_continuation_and_internal_dependencies() {
    let originals = vec![
        (vec![1, 0], b"AA\x01x".to_vec()),
        (vec![2, 0, 0], Vec::new()),
        (vec![1, 0], b"AA\x01y".to_vec()),
        (vec![2, 0, 0], Vec::new()),
    ];
    let raw = stored(b"AA\x01xAA\x01y");
    let mut chunks = originals.clone();
    chunks.extend([
        (vec![8, 2, 4, 1], raw[..7].to_vec()),
        (vec![8, 1, 4, 1], raw[7..].to_vec()),
    ]);
    let bytes = directory_fixture(&chunks, Some(4));
    let out = decoder().decompress(&bytes).unwrap();
    let OutputStructure::Framed { layout, .. } = out.structure else {
        panic!()
    };
    assert_eq!(layout.repeated_metadata.len(), 2);
    assert_eq!(layout.directory.unwrap().entries.len(), 6);
    for split in 0..=bytes.len() {
        assert!(collect_incremental(&bytes, split, 1).is_empty());
    }
    let mut chunks = originals.clone();
    let repeat_offset = 5 + chunks.iter().map(|(h, p)| chunk(h, p).len()).sum::<usize>();
    chunks.push((vec![8, 0, 1], b"AA\x01x".to_vec()));
    let mut header = vec![8, 3, 4, 1, 1];
    header.extend(var(repeat_offset as u64));
    header.push(1);
    chunks.push((header, stored(b"AA\x01y")));
    assert!(
        decoder()
            .decompress(&directory_fixture(&chunks, Some(4)))
            .is_ok()
    );
    chunks[5].0[5] = 5;
    assert!(matches!(
        decoder().decompress(&directory_fixture(&chunks, Some(4))),
        Err(FramedDecodeError::InvalidDictionaryReference)
    ));
    let mut chunks = originals;
    chunks.extend([
        (vec![8, 0, 1], b"AA\x01x".to_vec()),
        (vec![8, 0, 1], Vec::new()),
    ]);
    assert!(matches!(
        decoder().decompress(&directory_fixture(&chunks, Some(4))),
        Err(FramedDecodeError::InvalidMetadata)
    ));
}
#[test]
fn directory_requires_every_original_header_and_owned_metadata_ranges_survive_reuse() {
    let mut bytes = vec![0x91, 10, 66, 82, 4];
    bytes.extend(chunk(&[2, 0, 0], b"x"));
    let offset = bytes.len() as u8;
    bytes.extend(chunk(&[9], &[0]));
    bytes.extend(chunk(&[10], &[0, offset]));
    assert!(matches!(
        decoder().decompress(&bytes),
        Err(FramedDecodeError::InvalidDirectory)
    ));
    let bytes = full(&[
        chunk(&[1, 0], b"id\x01x"),
        chunk(&[2, 0, 0], b"data"),
        chunk(&[6, 0], b"AA\x00"),
    ]);
    let mut dst = vec![0];
    let mut d = decoder();
    let out = d.decompress_into(&bytes, &mut dst).unwrap();
    d.decompress(&full(&[])).unwrap();
    drop(d);
    dst.extend(vec![0; 65536]);
    assert_eq!(&dst[out.resources[0].data.clone()], b"data");
    assert_eq!(
        out.resources[0].metadata.as_ref().unwrap().name(),
        Some("x")
    );
}
#[cfg(not(feature = "no_std"))]
#[test]
fn reader_drains_member_completion_without_waiting_for_transport_eof() {
    use std::io::{self, BufRead, Read};
    #[derive(Debug)]
    struct OpenTransport {
        consumed: bool,
    }
    impl Read for OpenTransport {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            unreachable!()
        }
    }
    impl BufRead for OpenTransport {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.consumed {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Ok(&[0x3b])
            }
        }
        fn consume(&mut self, n: usize) {
            if n != 0 {
                self.consumed = true;
            }
        }
    }
    let mut d = auto();
    let mut r = d
        .framed_reader(OpenTransport { consumed: false }, Default::default())
        .unwrap();
    let mut events = 0;
    while r.next_event().unwrap().is_some() {
        events += 1;
    }
    assert_eq!(events, 3);
    assert!(r.next_event().unwrap().is_none());
}

#[test]
fn independent_shared_payload_reads_attached_prefix_bytes_in_wire_order() {
    let mut raw = raw_wire::Wire::window(22, false);
    raw.copy(3, 6, 3);
    raw.copy(3, 6, 3);
    let raw = raw.finish();
    let first = chunk(&[2, 0, 1], b"abc");
    let second_offset = 5 + first.len();
    let second = chunk(&[2, 0, 1], b"xyz");
    let mut header = vec![2, 3, 6, 2, 0, 5, 0];
    header.extend(var(second_offset as u64));
    header.push(0);
    let bytes = full(&[first, second, chunk(&header, &raw)]);
    let out = decoder().decompress(&bytes).unwrap();
    assert_eq!(out.resources[2].data, b"abcxyz");
    let mut raw = raw_wire::Wire::window(22, false);
    raw.copy(3, 3, 3);
    let raw = raw.finish();
    let mut header = external_header(2);
    header[2] = 3;
    let resolver = Resolver {
        calls: std::cell::Cell::new(0),
        bytes: b"xyz".to_vec(),
    };
    let bytes = full(&[chunk(&header, &raw)]);
    assert_eq!(
        decoder()
            .decompress_with_dictionaries(&resolver, &bytes)
            .unwrap()
            .resources[0]
            .data,
        b"xyz"
    );
}
#[test]
fn effective_dictionary_slot_overflow_and_serialized_content_are_validated() {
    let resolver = Resolver {
        calls: std::cell::Cell::new(0),
        bytes: vec![0x91, 0, 3, b'a', b'b', b'c', 0, 0],
    };
    let mut raw = raw_wire::Wire::window(22, false);
    raw.copy(3, 3, 3);
    let raw = raw.finish();
    let mut header = external_header(6);
    header[2] = 3;
    assert_eq!(
        decoder()
            .decompress_with_dictionaries(&resolver, &full(&[chunk(&header, &raw)]))
            .unwrap()
            .resources[0]
            .data,
        b"abc"
    );
    let mut header = vec![2, 3, 0, 16];
    for _ in 0..16 {
        header.extend([2, 3]);
        header.extend([7; 32]);
    }
    header.push(0);
    assert!(matches!(
        decoder().decompress_with_dictionaries(&resolver, &full(&[chunk(&header, &[0x3b])])),
        Err(FramedDecodeError::Dictionary {
            source: mbrotli::dictionary::DecodeDictionaryError::TooManyAttachments,
            ..
        })
    ));
}
#[test]
fn uncompressed_exact_output_overrun_reports_size_contract_not_numeric_limit() {
    let bytes = single(b"abc");
    let mut d = decoder();
    let mut s = d.start(OutputSize::Exact(2).into()).unwrap();
    let mut p = 0;
    loop {
        match s.process(&bytes[p..], &mut [0; 8], DecodeOperation::Finish) {
            Ok(progress) => p += progress.consumed,
            Err(failure) => {
                assert!(matches!(
                    failure.error,
                    FramedDecodeError::DeclaredSizeMismatch {
                        expected: 2,
                        actual: 3
                    }
                ));
                break;
            }
        }
    }
}
#[test]
fn checksum_fields_are_recorded_and_footer_sizes_are_checked_without_authentication() {
    let mut header = vec![2, 0, 3, 3];
    header.extend([0x55; 32]);
    let mut bytes = full(&[chunk(&header, b"payload")]);
    let size = bytes.len();
    bytes[size - 2] = size as u8;
    let out = decoder().decompress(&bytes).unwrap();
    assert!(out.resources[0].hidden);
    assert_eq!(out.resources[0].checksum, Some(DictionaryId([0x55; 32])));
    bytes[size - 2] += 1;
    assert!(matches!(
        decoder().decompress(&bytes),
        Err(FramedDecodeError::InvalidFooter)
    ));
    let mut last = vec![5, 0, 2, 3];
    last.extend([0x77; 32]);
    let out = decoder()
        .decompress(&full(&[chunk(&[3, 0, 0], b"first"), chunk(&last, b"last")]))
        .unwrap();
    assert_eq!(out.resources[0].checksum, Some(DictionaryId([0x77; 32])));
}
