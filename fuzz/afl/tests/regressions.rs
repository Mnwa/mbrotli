//! Replays every committed fuzz input through its target body.
//!
//! This is the other half of the fuzzing loop: AFL finds an input, `cargo afl
//! tmin` shrinks it, the minimised bytes land in `regressions/<target>/` and
//! from then on `cargo afl test` re-checks them. Because the bodies in
//! [`mbrotli_afl::targets`] carry no AFL dependency, the replay needs no
//! instrumented target or live fuzzer, although the package's AFL binaries
//! require the AFL runtime when Cargo builds all test targets.

use mbrotli_afl::{Context, targets};
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the `.bin` inputs committed for one target, in a stable order.
fn inputs_for(target: &str) -> Vec<PathBuf> {
    // Both directions consume the encoder's common parameter header + payload.
    let corpus = if target == "decode_roundtrip" {
        "params_roundtrip"
    } else {
        target
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("regressions")
        .join(corpus);
    let entries = fs::read_dir(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));

    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.expect("unreadable directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "bin"))
        .collect();
    paths.sort();
    paths
}

#[test]
fn c_encoded_payloads_roundtrip_at_every_quality_on_each_host_backend() {
    for level in mbrotli_afl::host_levels() {
        let context = Context {
            level,
            levels: vec![level],
        };
        for quality in 0..12 {
            for window in [0, 14] {
                for mode in 0..3 {
                    let mut input = vec![quality, window, 0, mode, 0, 0];
                    input.extend_from_slice(b"The quick brown fox jumps over the lazy dog. ");
                    input.extend_from_slice(&[0, 255, 128, 0, 1, 2, 3]);
                    mbrotli_afl::decode_targets::decode_roundtrip(&context, &input);
                }
            }
        }
    }
}

#[test]
fn c_to_rust_roundtrip_handles_empty_short_and_capped_payloads() {
    let context = Context::default();
    for input in [&[][..], &[11], &[11, 14, 0, 0, 0, 0], &[0; 7]] {
        mbrotli_afl::decode_targets::decode_roundtrip(&context, input);
    }
    for length in [64 * 1024, 64 * 1024 + 1, mbrotli_afl::MAX_PAYLOAD + 1] {
        let mut input = vec![11, 14, 0, 2, 24, 255];
        input.extend((0..length).map(|index| (index * 37) as u8));
        mbrotli_afl::decode_targets::decode_roundtrip(&context, &input);
    }
}

#[test]
fn arbitrary_decoder_bytes_do_not_panic_on_any_host_backend() {
    for level in mbrotli_afl::host_levels() {
        let context = Context {
            level,
            levels: vec![level],
        };
        // Fixed PRNG state keeps arbitrary-byte failures reproducible in replay.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for length in [0, 1, 2, 3, 7, 31, 256, 4096] {
            let mut input = Vec::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                input.push((state >> 32) as u8);
            }
            mbrotli_afl::decode_targets::decompress(&context, &input);
            mbrotli_afl::decode_targets::decode_streaming(&context, &input);
        }
    }
}

#[test]
fn experimental_targets_follow_feature_selection() {
    for name in [
        "serialized_dictionary",
        "framing",
        "decode_serialized",
        "framed_decode",
        "framed_roundtrip",
    ] {
        assert_eq!(
            targets::TARGETS.iter().any(|(target, _)| *target == name),
            cfg!(feature = "experimental"),
            "{name} must only be registered with experimental enabled"
        );
    }
}

#[test]
fn every_target_has_a_regression_corpus() {
    for &(name, _) in targets::TARGETS {
        assert!(
            !inputs_for(name).is_empty(),
            "no committed inputs for target {name}"
        );
    }
}

#[test]
fn every_committed_input_replays_without_violating_its_oracle() {
    let ctx = Context::default();
    let mut replayed = 0usize;

    for &(name, body) in targets::TARGETS {
        for path in inputs_for(name) {
            // Printed so a panic below names the input that caused it; test
            // output is only shown for failing tests.
            println!("replaying {}", path.display());
            let input =
                fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            body(&ctx, &input);
            replayed += 1;
        }
    }

    assert!(replayed > 0, "no regression inputs were replayed");
}

#[test]
fn decoder_timeout_inputs_remain_independent_across_repeated_calls() {
    let context = mbrotli_afl::Context::default();
    let cases: [&[u8]; 3] = [
        include_bytes!("../regressions/decode_dictionary/timeout-replay.bin"),
        include_bytes!("../regressions/decode_dictionary/timeout-replay-2.bin"),
        include_bytes!("../regressions/decode_dictionary/raw-empty-member.bin"),
    ];
    for _ in 0..10000 {
        for input in cases {
            mbrotli_afl::decode_targets::decode_dictionary(&context, input);
        }
    }
}
