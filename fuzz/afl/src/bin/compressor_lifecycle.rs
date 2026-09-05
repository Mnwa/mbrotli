//! Fuzzes the lifecycle of one compressor: reuse, failure, reconfiguration,
//! trimming and abandoned sessions in whatever order the input asks for.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::compressor_lifecycle(&ctx, data);
    });
}
