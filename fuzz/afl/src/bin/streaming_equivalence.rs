//! Chunk boundaries must not change the stream the adapters produce.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::streaming_equivalence`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::streaming_equivalence(&ctx, data);
    });
}
