//! Randomised legal settings must round-trip and stay deterministic.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::params_roundtrip`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::params_roundtrip(&ctx, data);
    });
}
