mod support;
use mbrotli::*;

#[test]
fn validated_configuration_and_exact_size_contracts_have_typed_errors() {
    for bits in [0, 9, 25, 255] {
        assert!(WindowLimit::standard(bits).is_err());
    }
    for bits in [0, 9, 63, 255] {
        assert!(WindowLimit::large(bits).is_err());
    }
    assert_eq!(WindowLimit::standard(24).unwrap().max_bits(), 24);
    assert!(!WindowLimit::standard(24).unwrap().allows_large());
    assert!(WindowLimit::large(62).unwrap().allows_large());
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let source = support::c_compress_native_one_shot(0, 22, b"hello");
    let config = DecodeStreamConfig::default().with_output_size(OutputSize::Exact(6));
    let mut session = decoder.start(config).unwrap();
    assert!(session.window().is_none());
    let failure = session
        .process(&source, &mut [0; 6], DecodeOperation::Finish)
        .unwrap_err();
    assert_eq!(failure.produced, 5);
    assert!(session.window().is_some());
    assert!(matches!(
        failure.into_error(),
        DecodeError::OutputSizeMismatch {
            expected: 6,
            actual: 5
        }
    ));
    assert!(matches!(
        session
            .process(&[], &mut [], DecodeOperation::Finish)
            .unwrap_err()
            .error,
        DecodeError::InvalidState
    ));
}

#[test]
fn finish_latches_the_final_suffix_before_needs_output() {
    let source = support::c_compress_native_one_shot(0, 22, b"hello world");
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    for change_operation in [false, true] {
        let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
        let progress = session
            .process(&source, &mut [], DecodeOperation::Finish)
            .unwrap();
        assert_eq!(progress.status, DecoderStatus::NeedsOutput);
        let remaining = &source[progress.consumed..];
        let (input, op) = if change_operation {
            (remaining, DecodeOperation::Process)
        } else {
            (&remaining[..remaining.len() - 1], DecodeOperation::Finish)
        };
        let error = session.process(input, &mut [0; 16], op).unwrap_err();
        assert_eq!((error.consumed, error.produced), (0, 0));
        assert!(matches!(error.error, DecodeError::InvalidState));
    }
}

#[test]
fn aggregate_input_and_workspace_limits_check_exact_boundaries() {
    let source = support::c_compress_native_one_shot(0, 22, b"abc");
    for limit in [0, source.len() - 1, source.len(), source.len() + 1] {
        let limits = DecodeLimits::default().with_max_input_bytes(Some(limit as u64));
        let mut decoder = Decompressor::new(DecoderConfig::default().with_limits(limits)).unwrap();
        let result = decoder.decompress(&source);
        if limit < source.len() {
            assert!(matches!(
                result,
                Err(DecodeError::InputLimitExceeded { .. })
            ));
        } else {
            assert_eq!(result.unwrap(), b"abc");
        }
    }
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert_eq!(decoder.decompress(&source).unwrap(), b"abc");
    assert!(decoder.retained_bytes() > 0);
    decoder
        .reconfigure(
            DecoderConfig::default()
                .with_limits(DecodeLimits::default().with_max_workspace_bytes(Some(0))),
        )
        .unwrap();
    assert!(matches!(
        decoder.decompress(&source),
        Err(DecodeError::MemoryLimitExceeded { limit: 0 })
    ));
    assert!(decoder.decompress(&[0x3b]).unwrap().is_empty());
}

#[test]
fn baseline_and_every_host_backend_decode_the_same_c_bytes() {
    let payload = b"backends share a scalar decoder until profiling justifies kernels".repeat(100);
    let compressed = support::c_compress_native_one_shot(11, 10, &payload);
    for (_, backend) in support::host_levels() {
        let mut decoder = Decompressor::builder(DecoderConfig::default())
            .with_backend(backend)
            .build()
            .unwrap();
        assert_eq!(decoder.decompress(&compressed).unwrap(), payload);
    }
}

#[test]
fn empty_raw_slots_count_and_raw_magic_is_never_sniffed() {
    use mbrotli::dictionary::{
        DecodeDictionary, DecodeDictionaryError, DecodeDictionaryLimits, DictionaryAttachment,
    };
    let empty = DecodeDictionary::new(&[], DecodeDictionaryLimits::default()).unwrap();
    assert_eq!((empty.attachment_count(), empty.retained_bytes()), (0, 0));
    let slots = [DictionaryAttachment::Raw(&[]); 16];
    assert_eq!(
        DecodeDictionary::new(&slots[..15], DecodeDictionaryLimits::default())
            .unwrap()
            .attachment_count(),
        15
    );
    assert!(matches!(
        DecodeDictionary::new(&slots, DecodeDictionaryLimits::default()),
        Err(DecodeDictionaryError::TooManyAttachments)
    ));
    let bytes = [0x91, 0, 0xff, 0xff, 0xff];
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Raw(&bytes)],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    assert_eq!(dictionary.retained_bytes(), bytes.len());
    for (limits, source_error) in [
        (
            DecodeDictionaryLimits {
                max_source_bytes: Some(4),
                max_owned_bytes: None,
            },
            true,
        ),
        (
            DecodeDictionaryLimits {
                max_source_bytes: None,
                max_owned_bytes: Some(4),
            },
            false,
        ),
    ] {
        let error =
            DecodeDictionary::new(&[DictionaryAttachment::Raw(&bytes)], limits).unwrap_err();
        assert_eq!(
            matches!(error, DecodeDictionaryError::SourceLimitExceeded { .. }),
            source_error
        );
        assert!(!error.to_string().is_empty());
    }
}
