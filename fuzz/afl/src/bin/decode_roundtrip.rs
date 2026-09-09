//! C encoder to native decoder round-trips across qualities 0–11.

fn main() {
    let context = mbrotli_afl::Context::default();
    afl::fuzz!(|data: &[u8]| mbrotli_afl::decode_targets::decode_roundtrip(&context, data));
}
