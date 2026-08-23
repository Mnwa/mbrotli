//! Randomised legal settings must round-trip and stay deterministic.

use mbrotli::Brotli;
use mbrotli_afl::{assert_round_trip, decode_case};

fn main() {
    let compressor = Brotli::default().compressor();
    afl::fuzz!(|input: &[u8]| {
        let case = decode_case(input);
        let bound = compressor
            .calculate_bound(&case.params, case.data.len())
            .expect("bound overflowed");
        let compressed = compressor
            .compress(case.params, case.data)
            .expect("compression failed");
        assert!(compressed.len() <= bound, "output exceeded the bound");
        let again = compressor
            .compress(case.params, case.data)
            .expect("compression failed");
        assert_eq!(compressed, again, "compression is not deterministic");
        assert_round_trip(case.data, &compressed);
    });
}
