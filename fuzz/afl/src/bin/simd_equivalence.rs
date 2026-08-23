//! Every SIMD backend must emit exactly the same bytes.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::simd_equivalence`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::simd_equivalence(&ctx, data);
    });
}
