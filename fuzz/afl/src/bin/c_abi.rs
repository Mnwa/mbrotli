//! The `mbrotli-ffi` C ABI against Google's one-shot C API.
//!
//! Thin AFL adapter; the body lives in [`mbrotli_afl::c_abi_targets::c_abi`] so a
//! finding can be replayed without an instrumented binary.

fn main() {
    let context = mbrotli_afl::Context::default();
    afl::fuzz!(|data: &[u8]| mbrotli_afl::c_abi_targets::c_abi(&context, data));
}
