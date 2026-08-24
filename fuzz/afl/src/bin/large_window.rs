//! RFC 9841 large-window streams must round-trip and stay deterministic.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::large_window`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::large_window(&ctx, data);
    });
}
