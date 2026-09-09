fn main() {
    let context = mbrotli_afl::Context::default();
    afl::fuzz!(|data: &[u8]| mbrotli_afl::decode_targets::decode_lifecycle(&context, data));
}
