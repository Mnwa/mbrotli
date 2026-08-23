//! Quality 0 must never panic and must always round-trip.

use mbrotli::Brotli;
use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
use mbrotli_afl::assert_round_trip;

fn main() {
    let compressor = Brotli::default().compressor();
    let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    afl::fuzz!(|data: &[u8]| {
        let bound = compressor
            .calculate_bound(&params, data.len())
            .expect("bound overflowed");
        let compressed = compressor
            .compress(params, data)
            .expect("compression failed");
        assert!(compressed.len() <= bound, "output exceeded the bound");
        assert_round_trip(data, &compressed);
    });
}
