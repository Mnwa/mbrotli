//! The encoder must stay byte identical to the pinned C reference.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::differential_c`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::differential_c(&ctx, data);
    });
}
