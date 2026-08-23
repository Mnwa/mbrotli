//! The slice entry point must respect an exact and a one-byte-short buffer.

use mbrotli::Brotli;
use mbrotli::compressor::BrotliCompressError;
use mbrotli_afl::decode_case;

fn main() {
    let compressor = Brotli::default().compressor();
    afl::fuzz!(|input: &[u8]| {
        let case = decode_case(input);
        let expected = compressor
            .compress(case.params, case.data)
            .expect("compression failed");

        let mut exact = vec![0u8; expected.len()];
        let written = compressor
            .compress_to_slice(case.params, case.data, &mut exact)
            .expect("an exactly sized buffer must be accepted");
        assert_eq!(written, expected.len(), "written length differs");
        assert_eq!(exact, expected, "slice output differs");

        if expected.is_empty() {
            return;
        }
        let mut short = vec![0u8; expected.len() - 1];
        let outcome = compressor.compress_to_slice(case.params, case.data, &mut short);
        assert!(
            matches!(outcome, Err(BrotliCompressError::OutputTooSmall)),
            "a short buffer must be reported, not truncated"
        );
    });
}
