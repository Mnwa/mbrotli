//! Replays every committed fuzz input through its target body.
//!
//! This is the other half of the fuzzing loop: AFL finds an input, `cargo afl
//! tmin` shrinks it, the minimised bytes land in `regressions/<target>/` and
//! from then on an ordinary `cargo test` re-checks them. Because the bodies in
//! [`mbrotli_afl::targets`] carry no AFL dependency, the replay needs no
//! instrumented binary and no fuzzer.

use mbrotli_afl::{Context, targets};
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the `.bin` inputs committed for one target, in a stable order.
fn inputs_for(target: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("regressions")
        .join(target);
    let entries = fs::read_dir(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));

    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.expect("unreadable directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "bin"))
        .collect();
    paths.sort();
    paths
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
