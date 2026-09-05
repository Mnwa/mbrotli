//! An RFC 9841 serialized dictionary must parse the way the reference does.
//!
//! Thin AFL adapter; the body lives in
//! [`mbrotli_afl::targets::serialized_dictionary`] so a finding can be replayed
//! without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::serialized_dictionary(&ctx, data);
    });
}
