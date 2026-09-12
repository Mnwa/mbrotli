#![cfg(all(
    feature = "decompression",
    feature = "experimental",
    not(feature = "no_std")
))]
use mbrotli::framing::*;
use std::cell::Cell;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::rc::Rc;

fn var(mut n: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        bytes.push(b | if n == 0 { 0 } else { 128 });
        if n == 0 {
            return bytes;
        }
    }
}
fn container(chunks: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut bytes = vec![0x91, 10, 66, 82, 4];
    let mut entries = Vec::new();
    let mut repeat = 0;
    for (header, payload) in chunks {
        let offset = bytes.len() as u64;
        let mut copied = var((header.len() + payload.len()) as u64);
        copied.extend_from_slice(header);
        if header[0] == 8 && repeat == 0 {
            repeat = offset;
        }
        if header[0] != 0 {
            entries.extend(var(offset));
            entries.extend(var(copied.len() as u64));
            entries.extend_from_slice(&copied);
        }
        bytes.extend(copied);
        bytes.extend_from_slice(payload);
    }
    let directory = bytes.len() as u64;
    let mut body = vec![9];
    body.extend(var(repeat));
    body.extend(entries);
    bytes.extend(var(body.len() as u64));
    bytes.extend(body);
    let mut footer = var(directory);
    footer.push(0);
    footer.reverse();
    bytes.extend(var((1 + footer.len()) as u64));
    bytes.push(10);
    bytes.extend(footer);
    bytes
}
fn decoder() -> FramedDecompressor {
    FramedDecompressor::new(Default::default()).unwrap()
}
fn payload<R: Read + Seek>(reader: &mut FramedSeekReader<'_, '_, R>, i: u64) -> Vec<u8> {
    let mut resource = reader.resource(ResourceIndex(i)).unwrap();
    assert_eq!(resource.index(), ResourceIndex(i));
    assert_eq!(resource.info().index(), ResourceIndex(i));
    let mut out = Vec::new();
    let mut byte = [0];
    assert_eq!(resource.read(&mut []).unwrap(), 0);
    while resource.read(&mut byte).unwrap() != 0 {
        out.push(byte[0]);
    }
    assert_eq!(resource.total_out(), out.len() as u64);
    assert_eq!(resource.read(&mut byte).unwrap(), 0);
    out
}
#[test]
fn arbitrary_order_repeat_early_drop_and_source_access() {
    let bytes = container(&[
        (&[2, 0, 1], b"abc"),
        (&[2, 0, 0], b""),
        (&[2, 0, 0], b"xyz"),
    ]);
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(reader.header(), ContainerHeader { flags: 4 });
    assert!(reader.footer().directory.is_some());
    assert!(reader.resources()[0].hidden());
    assert_eq!(reader.resources()[0].checksum(), None);
    assert_eq!(reader.resources()[0].decoded_size(), Some(3));
    assert_eq!(reader.resources()[0], reader.resources()[0].clone());
    assert!(reader.resource_info(ResourceIndex(u64::MAX)).is_none());
    assert!(matches!(
        reader.resource(ResourceIndex(3)),
        Err(FramedSeekError::ResourceNotFound)
    ));
    assert!(matches!(
        reader.resource_metadata(ResourceIndex(3)),
        Err(FramedSeekError::ResourceNotFound)
    ));
    assert!(
        reader
            .resource_metadata(ResourceIndex(0))
            .unwrap()
            .is_none()
    );
    assert!(
        reader
            .resource_footer_metadata(ResourceIndex(0))
            .unwrap()
            .is_none()
    );
    assert_eq!(reader.get_ref().get_ref(), &bytes);
    reader.get_mut().set_position(0);
    {
        let mut r = reader.resource(ResourceIndex(0)).unwrap();
        r.read_exact(&mut [0]).unwrap();
    }
    for i in [2, 0, 1, 2, 0] {
        assert_eq!(
            payload(&mut reader, i),
            [b"abc".as_slice(), b"", b"xyz"][i as usize]
        );
    }
    assert!(!format!("{reader:?}").is_empty());
    let inner = reader.into_inner();
    assert_eq!(inner.into_inner(), bytes);
    assert!(d.decompress(&bytes).is_ok());
}
#[path = "decode_support/wire.rs"]
mod raw_wire;
fn stored(payload: &[u8]) -> Vec<u8> {
    let mut wire = raw_wire::Wire::window(22, false);
    wire.raw(payload);
    wire.finish()
}
#[test]
fn metadata_continuation_partial_chunks_and_lazy_cache_match_sequential() {
    let raw = stored(b"id\x01xhello");
    let bytes = container(&[
        (&[1, 2, 4], &raw[..7]),
        (&[0], b"\0\0"),
        (&[3, 1, 2, 1], &raw[7..9]),
        (&[5, 1, 3, 0], &raw[9..]),
        (&[6, 0], b"AA\x01z"),
    ]);
    let expected = decoder().decompress(&bytes).unwrap();
    for backend in mbrotli::Backend::available() {
        let mut d = FramedDecompressor::builder(Default::default())
            .with_backend(backend)
            .build()
            .unwrap();
        let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
        assert_eq!(payload(&mut reader, 0), expected.resources[0].data);
        for _ in 0..2 {
            assert_eq!(
                reader
                    .resource_metadata(ResourceIndex(0))
                    .unwrap()
                    .unwrap()
                    .name(),
                Some("x")
            );
        }
        assert_eq!(
            reader.resource_footer_metadata(ResourceIndex(0)).unwrap(),
            expected.resources[0].footer_metadata.as_ref()
        );
        assert_eq!(payload(&mut reader, 0), b"hello");
    }
}
#[test]
fn dependencies_cover_resource_partial_chunk_metadata_and_serialized() {
    for (prefix, flags) in [
        (vec![(vec![2, 0, 1], b"prefix".to_vec())], 0),
        (
            vec![
                (vec![3, 0, 1], b"pre".to_vec()),
                (vec![5, 0, 0], b"fix".to_vec()),
            ],
            0,
        ),
        (
            vec![
                (vec![3, 0, 1], b"pre".to_vec()),
                (vec![5, 0, 0], b"fix".to_vec()),
            ],
            1,
        ),
        (vec![(vec![7, 0], b"AA\0".to_vec())], 1),
        (vec![(vec![2, 0, 1], vec![0x91, 0, 0, 0, 0])], 4),
    ] {
        let mut chunks: Vec<_> = prefix
            .iter()
            .map(|(h, p)| (h.as_slice(), p.as_slice()))
            .collect();
        let header = [2, 3, 0, 1, flags, 5, 0];
        chunks.push((&header, &[0x3b]));
        let bytes = container(&chunks);
        let expected = decoder().decompress(&bytes).unwrap();
        let mut d = decoder();
        let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
        let last = reader.resources().len() as u64 - 1;
        assert_eq!(
            payload(&mut reader, last),
            expected.resources.last().unwrap().data
        );
        let mut d = FramedDecompressor::new(
            FramedDecodeConfig::default()
                .with_internal_dictionaries(InternalDictionaryPolicy::Reject),
        )
        .unwrap();
        let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
        assert!(matches!(
            reader.resource(ResourceIndex(last)),
            Err(FramedSeekError::Decode(
                FramedDecodeError::InternalDictionaryReferencesDisabled
            ))
        ));
    }
}
struct Resolver;
impl DictionaryResolver for Resolver {
    fn resolve(&self, _: ExternalDictionaryRequest) -> Option<&[u8]> {
        Some(b"prefix")
    }
}
#[test]
fn external_dictionary_errors_preserve_typed_source_and_parent_reusability() {
    let mut header = vec![2, 3, 0, 1, 2, 3];
    header.extend([7; 32]);
    header.push(0);
    let bytes = container(&[(&header, &[0x3b]), (&[2, 0, 0], b"ok")]);
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
    {
        let mut resource = reader.resource(ResourceIndex(0)).unwrap();
        let error = resource.read(&mut [0]).unwrap_err();
        assert!(matches!(
            error.get_ref().unwrap().downcast_ref::<FramedSeekError>(),
            Some(FramedSeekError::Decode(
                FramedDecodeError::MissingDictionary { .. }
            ))
        ));
        assert!(resource.read(&mut [0]).is_err());
    }
    assert_eq!(payload(&mut reader, 1), b"ok");
    drop(reader);
    let mut reader = d
        .framed_seek_reader_with_dictionaries(&Resolver, Cursor::new(&bytes))
        .unwrap();
    assert_eq!(payload(&mut reader, 0), b"");
}
#[derive(Debug)]
struct Tracked {
    inner: Cursor<Vec<u8>>,
    reads: Rc<Cell<u64>>,
    forbid: Option<std::ops::Range<u64>>,
}
impl Read for Tracked {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let position = self.inner.position();
        if self.forbid.as_ref().is_some_and(|range| {
            position < range.end && position + bytes.len() as u64 > range.start
        }) {
            return Err(io::Error::other("forbidden payload"));
        }
        self.reads.set(self.reads.get() + 1);
        self.inner.read(bytes)
    }
}
impl Seek for Tracked {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}
#[test]
fn opening_and_unrelated_reads_skip_payload_and_drop_performs_no_io() {
    let huge = vec![0xff; 100000];
    let bytes = container(&[(&[2, 2, 0, 0], &huge), (&[2, 0, 0], b"ok")]);
    let reads = Rc::new(Cell::new(0));
    let source = Tracked {
        inner: Cursor::new(bytes),
        reads: reads.clone(),
        forbid: Some(20..100000),
    };
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(source).unwrap();
    assert_eq!(payload(&mut reader, 1), b"ok");
    let before = reads.get();
    drop(reader.resource(ResourceIndex(0)).unwrap());
    assert_eq!(reads.get(), before);
    let before = reads.get();
    drop(reader.into_inner());
    assert_eq!(reads.get(), before);
}
#[test]
fn directory_header_mismatch_is_detected_before_payload() {
    let mut bytes = container(&[(&[2, 0, 0], b"abc")]);
    bytes[8] = 1;
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(bytes)).unwrap();
    let error = reader
        .resource(ResourceIndex(0))
        .unwrap()
        .read(&mut [0])
        .unwrap_err();
    assert!(matches!(
        error.get_ref().unwrap().downcast_ref::<FramedSeekError>(),
        Some(FramedSeekError::DirectoryMismatch)
    ));
}
#[test]
fn missing_directory_malformed_offsets_truncations_and_order_are_distinct() {
    let mut d = decoder();
    for bytes in [b"\x91\x0aBR\0".as_slice(), b"\x91\x0aBR\x04\x03\x0a\0\0"] {
        assert!(matches!(
            d.framed_seek_reader(Cursor::new(bytes)),
            Err(FramedSeekError::CentralDirectoryRequired)
        ));
    }
    let bytes = container(&[(&[2, 0, 0], b"abc")]);
    for n in 0..bytes.len() {
        assert!(
            d.framed_seek_reader(Cursor::new(&bytes[..n])).is_err(),
            "{n}"
        );
    }
    for header in [
        &[4, 0, 0][..],
        &[5, 0, 0],
        &[6, 0],
        &[1, 0],
        &[2, 1, 0, 0],
        &[2, 3, 0, 1, 0, 5, 0],
    ] {
        assert!(
            d.framed_seek_reader(Cursor::new(container(&[(header, b"")])))
                .is_err()
        );
    }
    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() = 1;
    assert!(matches!(
        d.framed_seek_reader(Cursor::new(bad)),
        Err(FramedSeekError::Decode(FramedDecodeError::InvalidDirectory))
    ));
    let mut bad = bytes.clone();
    bad[0] = 0;
    assert!(matches!(
        d.framed_seek_reader(Cursor::new(bad)),
        Err(FramedSeekError::Decode(FramedDecodeError::InvalidSignature))
    ));
    let mut bad = bytes;
    bad[4] = 5;
    assert!(matches!(
        d.framed_seek_reader(Cursor::new(bad)),
        Err(FramedSeekError::Decode(
            FramedDecodeError::UnsupportedVersion(1)
        ))
    ));
    assert!(
        d.framed_seek_reader(Cursor::new(container(&[])))
            .unwrap()
            .resources()
            .is_empty()
    );
}
#[test]
fn limits_reset_per_operation_and_streaming_does_not_retain_target_payload() {
    let bytes = container(&[(&[2, 0, 0], b"abc"), (&[2, 0, 0], b"xyz")]);
    let limits = FramedDecodeLimits::default()
        .with_max_output_bytes(Some(3))
        .with_max_dictionary_bytes(Some(0));
    let mut d = FramedDecompressor::new(FramedDecodeConfig::default().with_limits(limits)).unwrap();
    let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
    for i in [0, 1, 0, 1] {
        assert_eq!(payload(&mut reader, i).len(), 3);
    }
    drop(reader);
    for limits in [
        FramedDecodeLimits::default().with_max_framing_bytes(Some(0)),
        FramedDecodeLimits::default().with_max_workspace_bytes(Some(0)),
        FramedDecodeLimits::default().with_max_resources(Some(1)),
        FramedDecodeLimits::default().with_max_chunks(Some(3)),
        FramedDecodeLimits::default().with_max_input_bytes(Some(1)),
    ] {
        let mut d =
            FramedDecompressor::new(FramedDecodeConfig::default().with_limits(limits)).unwrap();
        assert!(d.framed_seek_reader(Cursor::new(&bytes)).is_err());
    }
    let limits = FramedDecodeLimits::default().with_max_resource_bytes(Some(2));
    let mut d = FramedDecompressor::new(FramedDecodeConfig::default().with_limits(limits)).unwrap();
    let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
    assert!(
        reader
            .resource(ResourceIndex(0))
            .unwrap()
            .read_to_end(&mut Vec::new())
            .is_err()
    );
}
#[test]
fn repeated_metadata_is_transparent_and_original_fields_are_preserved() {
    let bytes = container(&[
        (&[1, 0], b"id\x01xAA\x01y"),
        (&[2, 0, 0], b"hello"),
        (&[8, 0, 1], b"id\x01x"),
    ]);
    let expected = decoder().decompress(&bytes).unwrap();
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
    assert_eq!(
        reader.resource_metadata(ResourceIndex(0)).unwrap(),
        expected.resources[0].metadata.as_ref()
    );
    assert_eq!(payload(&mut reader, 0), b"hello");
}

#[test]
fn actual_prefix_bytes_and_transitive_dependencies_are_delivered_only_to_target() {
    let mut raw = raw_wire::Wire::window(22, false);
    raw.copy(3, 3, 3);
    let raw = raw.finish();
    let first_header = [2, 3, 3, 1, 0, 5, 1];
    let second_offset = 5 + 7;
    let second_header = [2, 3, 3, 1, 0, second_offset, 0];
    let bytes = container(&[
        (&[2, 0, 1], b"abc"),
        (&first_header, &raw),
        (&second_header, &raw),
    ]);
    assert_eq!(
        decoder().decompress(&bytes).unwrap().resources[2].data,
        b"abc"
    );
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(bytes)).unwrap();
    for i in [2, 0, 1, 2] {
        assert_eq!(payload(&mut reader, i), b"abc");
    }
}

#[test]
fn forgotten_child_can_be_restarted_and_parent_drop_cancels_workspace() {
    let bytes = container(&[(&[2, 2, 5, 0], &stored(b"hello"))]);
    let mut d = decoder();
    {
        let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
        let mut resource = reader.resource(ResourceIndex(0)).unwrap();
        resource.read_exact(&mut [0]).unwrap();
        std::mem::forget(resource);
        assert_eq!(payload(&mut reader, 0), b"hello");
        let mut resource = reader.resource(ResourceIndex(0)).unwrap();
        resource.read_exact(&mut [0]).unwrap();
        std::mem::forget(resource);
    }
    assert_eq!(d.decompress(&bytes).unwrap().resources[0].data, b"hello");
}

#[test]
fn decoded_sizes_continuation_boundaries_and_metadata_limits_are_checked_on_access() {
    for chunks in [
        vec![(vec![2, 2, 4, 0], stored(b"hello"))],
        vec![(vec![2, 2, 6, 0], stored(b"hello"))],
        vec![
            (vec![2, 2, 5, 0], stored(b"hello")),
            (vec![2, 1, 0, 0], vec![]),
        ],
        vec![(vec![2, 2, 5, 0], stored(b"hello")[..8].to_vec())],
    ] {
        let borrowed: Vec<_> = chunks
            .iter()
            .map(|(h, p)| (h.as_slice(), p.as_slice()))
            .collect();
        let bytes = container(&borrowed);
        let mut d = decoder();
        let mut reader = d.framed_seek_reader(Cursor::new(bytes)).unwrap();
        assert!(
            reader
                .resource(ResourceIndex(0))
                .unwrap()
                .read_to_end(&mut Vec::new())
                .is_err()
        );
    }
    let bytes = container(&[(&[1, 0], b"id\x01x"), (&[2, 0, 0], b"ok")]);
    for limits in [
        FramedDecodeLimits::default().with_max_metadata_bytes(Some(3)),
        FramedDecodeLimits::default().with_max_metadata_fields(Some(0)),
        FramedDecodeLimits::default().with_max_decoded_bytes(Some(3)),
    ] {
        let mut d =
            FramedDecompressor::new(FramedDecodeConfig::default().with_limits(limits)).unwrap();
        let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
        assert!(reader.resource_metadata(ResourceIndex(0)).is_err());
        assert_eq!(payload(&mut reader, 0), b"ok");
    }
}

#[test]
fn io_errors_keep_original_source_and_error_variants_have_specific_messages() {
    use std::error::Error;
    let source = io::Error::new(io::ErrorKind::PermissionDenied, "source denied");
    let error = FramedSeekError::from(source);
    assert!(error.to_string().contains("source denied"));
    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let error = FramedSeekError::from(FramedDecodeError::InvalidDirectory);
    assert!(
        error
            .source()
            .unwrap()
            .downcast_ref::<FramedDecodeError>()
            .is_some()
    );
    assert_eq!(
        error.to_string(),
        "central directory does not match wire records"
    );
    for error in [
        FramedSeekError::CentralDirectoryRequired,
        FramedSeekError::ResourceNotFound,
        FramedSeekError::DirectoryMismatch,
    ] {
        assert!(error.source().is_none());
        assert!(!error.to_string().is_empty());
        assert!(!format!("{error:?}").is_empty());
    }
    let bytes = container(&[(&[2, 0, 0], b"abc")]);
    let source = Tracked {
        inner: Cursor::new(bytes),
        reads: Rc::new(Cell::new(0)),
        forbid: Some(0..5),
    };
    assert!(matches!(
        decoder().framed_seek_reader(source),
        Err(FramedSeekError::Io(_))
    ));
}

#[test]
fn footer_metadata_can_continue_a_resource_stream_and_prefix_metadata() {
    let raw = stored(b"helloAA\x01x");
    let bytes = container(&[(&[2, 2, 5, 0], &raw[..8]), (&[6, 1, 4], &raw[8..])]);
    let expected = decoder().decompress(&bytes).unwrap();
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(bytes)).unwrap();
    assert_eq!(
        reader.resource_footer_metadata(ResourceIndex(0)).unwrap(),
        expected.resources[0].footer_metadata.as_ref()
    );
    assert_eq!(payload(&mut reader, 0), b"hello");

    let mut raw = raw_wire::Wire::window(22, false);
    raw.copy(4, 4, 4);
    let bytes = container(&[
        (&[7, 0], b"id\x01x"),
        (&[1, 3, 4, 1, 1, 5], &raw.finish()),
        (&[2, 0, 0], b"hello"),
    ]);
    // Global reserved fields are rejected if used as a dependency.
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(bytes)).unwrap();
    assert!(reader.resource_metadata(ResourceIndex(0)).is_err());
    assert_eq!(payload(&mut reader, 0), b"hello");
}

#[test]
fn nonminimal_copied_and_footer_headers_remain_wire_identical() {
    // A nonminimal data length is copied verbatim into the directory.
    let bytes = b"\x91\x0aBR\x04\x84\x00\x02\x00\x00x\x09\x09\x00\x05\x05\x84\x00\x02\x00\x00\x83\x00\x0a\x00\x0b";
    assert_eq!(decoder().decompress(bytes).unwrap().resources[0].data, b"x");
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(bytes)).unwrap();
    assert_eq!(payload(&mut reader, 0), b"x");
}

#[test]
fn large_resource_streams_under_small_workspace_and_open_input_budgets() {
    let data = vec![b'x'; 2 << 20];
    let bytes = container(&[(&[2, 0, 0], &data)]);
    let limits = FramedDecodeLimits::default()
        .with_max_workspace_bytes(Some(65536))
        .with_max_dictionary_bytes(Some(0));
    let mut d = FramedDecompressor::new(FramedDecodeConfig::default().with_limits(limits)).unwrap();
    let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
    assert_eq!(
        io::copy(
            &mut reader.resource(ResourceIndex(0)).unwrap(),
            &mut io::sink()
        )
        .unwrap(),
        data.len() as u64
    );
    drop(reader);
    d.reconfigure(
        FramedDecodeConfig::default().with_limits(limits.with_max_input_bytes(Some(100))),
    )
    .unwrap();
    assert!(d.framed_seek_reader(Cursor::new(&bytes)).is_ok());
}

#[test]
fn forgotten_parent_preserves_owner_abandonment_and_explicit_recovery() {
    let bytes = container(&[(&[2, 2, 5, 0], &stored(b"hello"))]);
    let mut d = decoder();
    let mut reader = d.framed_seek_reader(Cursor::new(&bytes)).unwrap();
    let mut resource = reader.resource(ResourceIndex(0)).unwrap();
    resource.read_exact(&mut [0]).unwrap();
    std::mem::forget(resource);
    std::mem::forget(reader);
    assert!(matches!(
        d.start(Default::default()),
        Err(FramedDecodeError::AbandonedSession)
    ));
    assert!(matches!(
        d.framed_seek_reader(Cursor::new(&bytes)),
        Err(FramedSeekError::Decode(FramedDecodeError::AbandonedSession))
    ));
    d.recover();
    assert_eq!(d.decompress(&bytes).unwrap().resources[0].data, b"hello");
}

#[test]
fn directory_gaps_reject_omitted_content_and_count_padding_chunks() {
    let omitted = b"\x91\x0aBR\x04\x04\x02\x00\x00a\x04\x02\x00\x00b\x08\x09\x00\x0a\x04\x04\x02\x00\x00\x03\x0a\x00\x0f";
    assert!(matches!(
        decoder().framed_seek_reader(Cursor::new(omitted)),
        Err(FramedSeekError::Decode(FramedDecodeError::InvalidDirectory))
    ));
    let mut bytes = container(&[(&[2, 0, 0], b"x")]);
    let footer = bytes.len() - 4;
    bytes.splice(footer..footer, [0, 0]);
    assert!(decoder().decompress(&bytes).is_ok());
    for limit in [4, 5] {
        let config = FramedDecodeConfig::default()
            .with_limits(FramedDecodeLimits::default().with_max_chunks(Some(limit)));
        let mut d = FramedDecompressor::new(config).unwrap();
        assert_eq!(
            d.framed_seek_reader(Cursor::new(&bytes)).is_ok(),
            limit == 5
        );
    }
}
