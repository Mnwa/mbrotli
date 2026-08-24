//! An RFC 9841 shared context must be sound however it is built and read.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::shared_context`]
//! so a finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::shared_context(&ctx, data);
    });
}
