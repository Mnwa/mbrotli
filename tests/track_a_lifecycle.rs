//! Regression tests for final-output delivery and retention across API shapes.

use mbrotli::{Compressor, EncoderConfig, EncoderStatus, Operation, Quality, RetentionPolicy};
use std::io::{Read, Write};

#[test]
fn backends_are_opaque_host_validated_and_have_stable_diagnostics() {
    let backends = mbrotli::Backend::available();
    assert!(backends.contains(&mbrotli::Backend::SCALAR));
    assert!(backends.contains(&mbrotli::Backend::default()));
    for backend in backends {
        assert_eq!(format!("{backend}"), backend.name());
        assert_eq!(format!("{backend:?}"), backend.name());
        let mut compressor = Compressor::builder(EncoderConfig::default())
            .with_backend(backend)
            .build()
            .expect("backend");
        assert!(
            !compressor
                .compress(b"payload")
                .expect("compress")
                .is_empty()
        );
    }
}

#[test]
fn a_session_finishes_only_after_its_last_byte_is_delivered() {
    let mut compressor = Compressor::new(EncoderConfig::default()).expect("config");
    let mut session = compressor.start(Default::default()).expect("session");
    let progress = session
        .process(b"payload", &mut [], Operation::Finish)
        .expect("finish");
    assert_eq!(progress.status, EncoderStatus::NeedsOutput);
    assert!(!session.is_finished());
    loop {
        let progress = session
            .process(&[], &mut [0], Operation::Finish)
            .expect("drain");
        assert_eq!(
            session.is_finished(),
            progress.status == EncoderStatus::Finished
        );
        if session.is_finished() {
            break;
        }
    }
}

#[test]
fn retention_is_applied_after_every_streaming_api_and_abandonment() {
    for policy in [
        RetentionPolicy::ReleaseAll,
        RetentionPolicy::Bounded { max_bytes: 0 },
    ] {
        for quality in [Quality::Q0, Quality::Q1, Quality::Q5, Quality::Q11] {
            let mut compressor =
                Compressor::builder(EncoderConfig::default().with_quality(quality))
                    .with_retention(policy)
                    .build()
                    .expect("config");
            {
                let mut session = compressor.start(Default::default()).expect("session");
                assert_eq!(
                    session
                        .process(b"payload", &mut [0; 1024], Operation::Finish)
                        .expect("finish")
                        .status,
                    EncoderStatus::Finished
                );
            }
            assert_eq!(
                compressor.retained_bytes(),
                0,
                "session {quality:?} {policy:?}"
            );
            {
                let mut writer = compressor
                    .writer(Vec::new(), Default::default())
                    .expect("writer");
                writer.write_all(b"payload").expect("write");
                writer.finish().expect("finish");
            }
            assert_eq!(
                compressor.retained_bytes(),
                0,
                "writer {quality:?} {policy:?}"
            );
            {
                let mut reader = compressor
                    .reader(&b"payload"[..], Default::default())
                    .expect("reader");
                reader.read_to_end(&mut Vec::new()).expect("read");
            }
            assert_eq!(
                compressor.retained_bytes(),
                0,
                "reader {quality:?} {policy:?}"
            );
            {
                let mut session = compressor.start(Default::default()).expect("session");
                session
                    .process(b"payload", &mut [], Operation::Process)
                    .expect("process");
            }
            assert_eq!(
                compressor.retained_bytes(),
                0,
                "abandon {quality:?} {policy:?}"
            );
        }
    }
}
