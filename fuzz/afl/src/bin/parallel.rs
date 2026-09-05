//! Fuzzes independent parts, deterministic task order, and worker/backend reuse.
use mbrotli_afl::{Context, targets};
fn main() {
    let context = Context::default();
    afl::fuzz!(|data: &[u8]| {
        targets::parallel(&context, data);
    });
}
