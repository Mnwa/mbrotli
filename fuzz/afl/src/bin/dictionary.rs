//! An RFC 9841 prepared dictionary must be sound however it is built and used.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::dictionary`]
//! so a finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::dictionary(&ctx, data);
    });
}
