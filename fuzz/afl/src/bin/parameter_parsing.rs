//! Parameter parsing must reject illegal settings and never panic.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::targets::parameter_parsing`] so a
//! finding can be replayed without an instrumented binary.

use mbrotli_afl::{Context, targets};

fn main() {
    let ctx = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::parameter_parsing(&ctx, data);
    });
}
