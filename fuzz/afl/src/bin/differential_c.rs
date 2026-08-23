//! The encoder must stay byte identical to the pinned C reference.

use mbrotli::Brotli;
use mbrotli_afl::{FAST_QUALITIES, c_compress, decode_case};
use std::ffi::c_int;

fn main() {
    let compressor = Brotli::default().compressor();
    afl::fuzz!(|input: &[u8]| {
        let case = decode_case(input);
        let quality = usize::from(case.params.quality()) as c_int;
        let lgwin = usize::from(case.params.lgwin()) as c_int;
        let expected = c_compress(quality, lgwin, case.data);
        let actual = compressor
            .compress(case.params, case.data)
            .expect("compression failed");
        assert_eq!(actual, expected, "the Rust and C encoders disagree");
        assert!(
            FAST_QUALITIES
                .iter()
                .any(|&q| usize::from(q) == quality as usize)
        );
    });
}
