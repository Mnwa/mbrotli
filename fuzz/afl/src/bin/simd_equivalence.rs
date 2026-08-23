//! Every SIMD backend must emit exactly the same bytes.

use mbrotli::Brotli;
use mbrotli_afl::{decode_case, host_levels};

fn main() {
    let levels = host_levels();
    afl::fuzz!(|input: &[u8]| {
        let case = decode_case(input);
        let mut reference: Option<Vec<u8>> = None;
        for &level in &levels {
            let actual = Brotli::from(level)
                .compressor()
                .compress(case.params, case.data)
                .expect("compression failed");
            match &reference {
                None => reference = Some(actual),
                Some(expected) => assert_eq!(&actual, expected, "backends disagree"),
            }
        }
    });
}
