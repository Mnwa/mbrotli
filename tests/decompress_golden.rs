//! Pure Rust fixture replay, also runnable under Miri without executing C FFI.
use mbrotli::{DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus, Decompressor};
const FIXTURES: [(&[u8], &[u8]); 3] = [
    (
        include_bytes!("fixtures/decompress/official-cli.br"),
        include_bytes!("fixtures/decompress/continuation-standard.raw"),
    ),
    (
        include_bytes!("fixtures/decompress/continuation-standard.br"),
        include_bytes!("fixtures/decompress/continuation-standard.raw"),
    ),
    (
        include_bytes!("fixtures/decompress/continuation-large.br"),
        include_bytes!("fixtures/decompress/continuation-large.raw"),
    ),
];
#[test]
fn pinned_c_continuations_decode_in_every_profile_and_chunk_size() {
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    for (compressed, payload) in FIXTURES {
        assert_eq!(decoder.decompress(compressed).unwrap(), payload);
        let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
        let mut cursor = 0;
        let mut decoded = Vec::new();
        loop {
            let end = (cursor + 1).min(compressed.len());
            let mut output = [0; 3];
            let progress = session
                .process(
                    &compressed[cursor..end],
                    &mut output,
                    if end == compressed.len() {
                        DecodeOperation::Finish
                    } else {
                        DecodeOperation::Process
                    },
                )
                .unwrap();
            cursor += progress.consumed;
            decoded.extend_from_slice(&output[..progress.produced]);
            if progress.status == DecoderStatus::Finished {
                break;
            }
            assert!(progress.consumed > 0 || progress.produced > 0);
        }
        assert_eq!(decoded, payload);
    }
}

#[test]
fn parallel_encoder_golden_decodes_without_an_encoder_or_std_dependency() {
    let compressed = include_bytes!("fixtures/decompress/parallel.br");
    let expected = b"parallel parts preserve the common Brotli stream state. ".repeat(5000);
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert_eq!(decoder.decompress(compressed).unwrap(), expected);
}
