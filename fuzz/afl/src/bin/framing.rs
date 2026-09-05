//! Bounded RFC framing sequences and caller-chunking equivalence.
use mbrotli_afl::{Context, targets};
fn main() {
    let context = Context::default();
    afl::fuzz!(|data: &[u8]| targets::framing(&context, data));
}
