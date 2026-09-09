//! Reproducible heavy release checks: cargo test --release --test
//! decompress_heavy -- --ignored --test-threads=1. Peak live history < 3 GiB.
#[path = "decode_support/c_decoder.rs"]
mod c_decoder;
#[path = "decode_support/wire.rs"]
mod wire;
use mbrotli::{DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus, Decompressor};
use wire::Wire;

fn feed(
    session: &mut mbrotli::DecoderSession<'_, '_>,
    c: &mut c_decoder::Decoder,
    encoded: &[u8],
    count: &mut u64,
    hash: &mut u64,
) {
    let mut cursor = 0;
    let mut c_cursor = 0;
    loop {
        let mut output = [0; 65536];
        let mut reference = [0; 65536];
        let progress = session
            .process(&encoded[cursor..], &mut output, DecodeOperation::Process)
            .unwrap();
        cursor += progress.consumed;
        let mut c_written = 0;
        let mut c_finished = false;
        while c_written < progress.produced
            || (progress.status == DecoderStatus::Finished && !c_finished)
            || c_cursor < cursor
        {
            let (consumed, produced, finished) = c.process(
                &encoded[c_cursor..cursor],
                &mut reference[c_written..progress.produced],
            );
            c_cursor += consumed;
            c_written += produced;
            c_finished = finished;
            if consumed == 0 && produced == 0 {
                break;
            }
        }
        assert_eq!(c_written, progress.produced);
        assert_eq!(&output[..progress.produced], &reference[..c_written]);
        for &byte in &output[..progress.produced] {
            *hash = hash.wrapping_mul(0x100000001b3) ^ u64::from(byte);
        }
        *count += progress.produced as u64;
        if progress.status != DecoderStatus::NeedsOutput {
            break;
        }
    }
    assert_eq!((cursor, c_cursor), (encoded.len(), encoded.len()));
}

#[test]
#[ignore = "release job: 2 GiB peak history, execute with --ignored --test-threads=1"]
fn real_far_references_use_every_c_large_window() {
    for bits in 25..=30 {
        let distance = (1u64 << bits) - 16;
        let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
        let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
        let mut reference = c_decoder::Decoder::default();
        let mut count = 0;
        let mut hash = 0xcbf29ce484222325;
        let mut wire = Wire::window(bits, true);
        let mut left = distance;
        let chunk = [0x6b; 65536];
        while left != 0 {
            let length = left.min(1 << 24) as usize;
            wire.raw_header(length);
            feed(
                &mut session,
                &mut reference,
                &wire.take(),
                &mut count,
                &mut hash,
            );
            let mut block_left = length;
            while block_left != 0 {
                let n = block_left.min(chunk.len());
                if count == 0 {
                    let mut first = chunk;
                    first[..16].copy_from_slice(b"far-history-mark");
                    feed(
                        &mut session,
                        &mut reference,
                        &first[..n],
                        &mut count,
                        &mut hash,
                    );
                } else {
                    feed(
                        &mut session,
                        &mut reference,
                        &chunk[..n],
                        &mut count,
                        &mut hash,
                    );
                }
                block_left -= n;
            }
            left -= length as u64;
        }
        // The explicit fixture symbol proves the distance actually used; C
        // decoding independently validates the wire representation and history.
        wire.copy(16, distance, 16);
        let encoded = wire.finish();
        let mut output = [0; 16];
        let progress = session
            .process(&encoded, &mut output, DecodeOperation::Finish)
            .unwrap();
        assert_eq!(progress.status, DecoderStatus::Finished);
        assert_eq!(&output, b"far-history-mark");
        let mut reference_output = [0; 16];
        let (consumed, produced, finished) = reference.process(&encoded, &mut reference_output);
        assert_eq!(reference_output, output);
        assert_eq!((consumed, produced, finished), (encoded.len(), 16, true));
        assert_eq!(session.total_out(), distance + 16);
        eprintln!(
            "WBITS={bits}, actual distance={distance}, decoded={}, history hash={hash:016x}",
            session.total_out()
        );
    }
}

#[test]
#[ignore = "release job: >4 GiB decoded, bounded 1 KiB history and 64 KiB buffers"]
fn cumulative_counters_cross_four_gib_with_a_rolling_hash() {
    let mut wire = Wire::window(10, false);
    wire.metadata(b"counter test");
    wire.raw(b"x");
    // 257 * 16 MiB exceeds u32::MAX without needing a large compressed fixture.
    for _ in 0..257 {
        wire.copy(1 << 24, 1, 1 << 24);
    }
    let compressed = wire.finish();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
    let mut reference = c_decoder::Decoder::default();
    let mut count = 0;
    let mut hash = 0xcbf29ce484222325;
    feed(
        &mut session,
        &mut reference,
        &compressed,
        &mut count,
        &mut hash,
    );
    assert!(session.is_finished());
    assert_eq!(count, 1 + 257 * (1u64 << 24));
    assert_eq!(count, session.total_out());
    assert_eq!(session.total_in(), compressed.len() as u64);
    eprintln!(
        "decoded={count}, compressed={}, hash={hash:016x}",
        compressed.len()
    );
}
