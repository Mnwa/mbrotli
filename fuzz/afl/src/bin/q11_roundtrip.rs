//! Quality 11 must never panic and must always round-trip.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::q11_roundtrip`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::q11_roundtrip(&ctx, data);
    });
}
