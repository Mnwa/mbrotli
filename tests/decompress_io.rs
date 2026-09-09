#![cfg(not(feature = "no_std"))]

mod support;
use mbrotli::{DecodeStreamConfig, DecoderConfig, Decompressor};
use std::io::{self, Read, Write};

#[test]
fn readers_and_writers_decode_the_same_c_payload() {
    let payload = b"streaming decoding with reader and writer\n".repeat(2000);
    let compressed = support::c_compress_native_one_shot(9, 10, &payload);
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    {
        let mut reader = decoder
            .reader(compressed.as_slice(), DecodeStreamConfig::default())
            .unwrap();
        let mut decoded = Vec::new();
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, payload);
        assert!(reader.is_finished());
        assert!(reader.into_parts().finished);
    }
    let mut writer = decoder
        .writer(Vec::new(), DecodeStreamConfig::default())
        .unwrap();
    for byte in &compressed {
        writer.write_all(&[*byte]).unwrap();
        writer.flush().unwrap();
    }
    writer.try_finish().unwrap();
    assert!(writer.is_finished());
    assert_eq!(writer.finish().unwrap(), payload);
}

#[test]
fn reader_preserves_read_ahead_and_defers_a_truncation_error() {
    let first = support::c_compress_native_one_shot(0, 22, b"first");
    let mut combined = first.clone();
    combined.extend_from_slice(b"trailing bytes");
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    {
        let mut reader = decoder
            .reader(combined.as_slice(), DecodeStreamConfig::default())
            .unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert_eq!(reader.get_ref().len(), combined.len());
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        assert_eq!(output, b"first");
        let parts = reader.into_parts();
        assert_eq!(parts.unread_input, b"trailing bytes");
        assert!(parts.reader.is_empty());
    }
    let truncated = &first[..first.len() - 1];
    let mut reader = decoder
        .reader(truncated, DecodeStreamConfig::default())
        .unwrap();
    let error = reader.read_to_end(&mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert!(error.get_ref().unwrap().is::<mbrotli::DecodeError>());
    assert!(reader.read(&mut [0]).is_err());
}

#[derive(Debug, Default)]
struct RetrySink {
    bytes: Vec<u8>,
    block: bool,
    flush_block: bool,
}
impl Write for RetrySink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.block {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let length = bytes.len().min(3);
        self.bytes.extend_from_slice(&bytes[..length]);
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.flush_block {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn sink_errors_preserve_accepted_input_and_finalization_can_retry() {
    let payload = b"retrying output must not duplicate decoded bytes".repeat(1000);
    let compressed = support::c_compress_native_one_shot(4, 10, &payload);
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let mut writer = decoder
        .writer(
            RetrySink {
                block: true,
                ..Default::default()
            },
            DecodeStreamConfig::default(),
        )
        .unwrap();
    let consumed = writer.write(&compressed).unwrap();
    assert!(consumed > 0);
    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    writer.get_mut().block = false;
    writer.write_all(&compressed[consumed..]).unwrap();
    writer.get_mut().flush_block = true;
    let mut writer = writer.finish().unwrap_err().into_inner();
    assert_eq!(
        writer.write(b"new").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    writer.get_mut().flush_block = false;
    writer.try_finish().unwrap();
    writer.try_finish().unwrap();
    assert_eq!(writer.get_ref().bytes, payload);
}

#[test]
fn writer_refuses_tail_and_reports_incomplete_final_input() {
    let compressed = support::c_compress_native_one_shot(0, 22, b"payload");
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    {
        let mut writer = decoder
            .writer(Vec::new(), DecodeStreamConfig::default())
            .unwrap();
        writer.write_all(&compressed).unwrap();
        assert_eq!(
            writer.write(b"tail").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    let mut writer = decoder
        .writer(Vec::new(), DecodeStreamConfig::default())
        .unwrap();
    writer
        .write_all(&compressed[..compressed.len() - 1])
        .unwrap();
    assert_eq!(
        writer.try_finish().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert!(writer.try_finish().is_err());
}

#[test]
fn codec_failure_preserves_payload_before_the_error() {
    let payload = b"payload before a malformed next member";
    let mut compressed = support::c_compress_native_one_shot(0, 22, payload);
    compressed.push(0xbb); // Invalid padding in a second empty member.
    let config = DecoderConfig::default().with_member_mode(mbrotli::MemberMode::Concatenated);
    let mut decoder = Decompressor::new(config).unwrap();
    let mut writer = decoder
        .writer(Vec::new(), DecodeStreamConfig::default())
        .unwrap();
    assert_eq!(writer.write(&compressed).unwrap(), compressed.len());
    assert_eq!(
        writer.flush().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    // Retrying delivery may never retry the failed codec or lose its payload.
    assert!(writer.flush().is_err());
    assert_eq!(writer.get_ref(), payload);
}

#[test]
fn dictionary_adapters_borrow_one_immutable_payload() {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let prefix = b"a prefix supplying dictionary references, ".repeat(30);
    let payload = &prefix[..300];
    let compressed =
        support::c_compress_with_prefixes(support::CParams::new(5, 22), &[&prefix], payload);
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Raw(&prefix)],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    {
        let mut reader = decoder
            .reader_with_dictionary(
                &dictionary,
                compressed.as_slice(),
                DecodeStreamConfig::default(),
            )
            .unwrap();
        assert_eq!(reader.get_mut().len(), compressed.len());
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, payload);
    }
    let mut writer = decoder
        .writer_with_dictionary(&dictionary, Vec::new(), DecodeStreamConfig::default())
        .unwrap();
    writer.write_all(&compressed).unwrap();
    assert_eq!(writer.finish().unwrap(), payload);
}

#[test]
fn public_errors_keep_their_type_inside_io_errors() {
    use mbrotli::{DecodeError, DecodeFailure, InvalidDataKind};
    use std::error::Error;
    let failure = DecodeFailure {
        error: InvalidDataKind::Distance.into(),
        consumed: 2,
        produced: 3,
    };
    assert!(failure.source().unwrap().is::<DecodeError>());
    assert_eq!(failure.to_string(), "invalid Brotli distance");
    for (error, kind) in [
        (DecodeError::AllocationFailed, io::ErrorKind::OutOfMemory),
        (DecodeError::InternalInvariant, io::ErrorKind::Other),
        (
            DecodeError::OutputTooSmall { written: 4 },
            io::ErrorKind::InvalidInput,
        ),
    ] {
        let wrapped: io::Error = error.into();
        assert_eq!(wrapped.kind(), kind);
        assert!(wrapped.get_ref().unwrap().is::<DecodeError>());
    }
}

#[test]
fn source_and_sink_faults_at_every_byte_preserve_progress() {
    #[derive(Debug)]
    struct FaultIo {
        bytes: Vec<u8>,
        cursor: usize,
        at: usize,
        fired: bool,
        kind: io::ErrorKind,
        zero: bool,
    }
    impl Read for FaultIo {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if !self.fired && self.cursor == self.at {
                self.fired = true;
                return Err(self.kind.into());
            }
            let bound = if self.fired {
                self.bytes.len()
            } else {
                self.at
            };
            let count = output.len().min(3).min(bound - self.cursor);
            output[..count].copy_from_slice(&self.bytes[self.cursor..self.cursor + count]);
            self.cursor += count;
            Ok(count)
        }
    }
    impl Write for FaultIo {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            if !self.fired && self.bytes.len() == self.at {
                self.fired = true;
                return if self.zero {
                    Ok(0)
                } else {
                    Err(self.kind.into())
                };
            }
            let bound = if self.fired {
                usize::MAX
            } else {
                self.at - self.bytes.len()
            };
            let count = input.len().min(3).min(bound);
            self.bytes.extend_from_slice(&input[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let payload = b"partial partial partial output, with no duplicated bytes";
    let compressed = support::c_compress_native_one_shot(5, 22, payload);
    for kind in [
        io::ErrorKind::Interrupted,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::Other,
    ] {
        for at in 0..=compressed.len() {
            let source = FaultIo {
                bytes: compressed.clone(),
                cursor: 0,
                at,
                fired: false,
                kind,
                zero: false,
            };
            let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
            let mut reader = decoder
                .reader(source, DecodeStreamConfig::default())
                .unwrap();
            let mut decoded = Vec::new();
            for _ in 0..compressed.len() + payload.len() + 10 {
                let mut buffer = [0; 2];
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => decoded.extend_from_slice(&buffer[..n]),
                    Err(error) => assert_eq!(error.kind(), kind),
                }
            }
            assert!(reader.is_finished());
            assert_eq!(decoded, payload);
        }
        for zero in [false, true] {
            for at in 0..=payload.len() {
                let sink = FaultIo {
                    bytes: Vec::new(),
                    cursor: 0,
                    at,
                    fired: false,
                    kind,
                    zero,
                };
                let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
                let mut writer = decoder.writer(sink, DecodeStreamConfig::default()).unwrap();
                let mut cursor = 0;
                for _ in 0..compressed.len() + payload.len() + 10 {
                    let result = if cursor == compressed.len() {
                        writer.try_finish().map(|()| 0)
                    } else {
                        writer.write(&compressed[cursor..(cursor + 3).min(compressed.len())])
                    };
                    match result {
                        Ok(n) => cursor += n,
                        Err(error) => assert_eq!(
                            error.kind(),
                            if zero { io::ErrorKind::WriteZero } else { kind }
                        ),
                    }
                    if writer.is_finished() {
                        break;
                    }
                }
                assert!(writer.is_finished());
                assert_eq!(writer.get_ref().bytes, payload);
            }
        }
    }
}
