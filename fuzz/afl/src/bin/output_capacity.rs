//! The slice entry point must respect an exact and a one-byte-short buffer.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::output_capacity`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::output_capacity(&ctx, data);
    });
}
