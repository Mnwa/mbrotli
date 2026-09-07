#!/bin/sh
# Reduces the prepared seed corpora to a coverage-equivalent subset.
#
# prepare-seeds.sh emits every payload under several parameter headers, and
# most of those headers reach the same code. afl-cmin keeps one seed per
# coverage profile, which is what the fuzzer should be cycling through. Expect
# the file count to drop sharply and the byte count barely at all: the large
# fixtures do carry coverage the small ones miss, so they survive. Per-iteration
# cost is bounded by MAX_PAYLOAD in src/lib.rs, not by this script.
#
# Run this after prepare-seeds.sh and after `cargo afl build --release`. Set
# TARGET_DIR to measure coverage with another build, for example the
# target/stable/release tree campaign.sh produces. The original corpora are
# kept as seeds/<corpus>.raw so that nothing is destroyed by a minimisation
# that turns out to be too aggressive.
#
# afl-cmin measures coverage with `afl-showmap -I`, whose folder mode stalls
# against these binaries: they are persistent-mode with a deferred forkserver,
# and every input then blocks until the -t timeout expires instead of running.
# The symptom is roughly six seconds per seed and an empty output directory.
# Disabling the forkserver makes showmap exec the target once per input, the
# way its single-file mode already does; the captured edge sets are identical
# and the whole corpus takes well under a second.
#
# Usage: [TARGET_DIR=target/stable/release] fuzz/afl/minimise-seeds.sh

set -eu

AFL_NO_FORKSRV=1
export AFL_NO_FORKSRV

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

# The target used to measure coverage for each corpus. Any target reading the
# same input shape will do; the generic and parameter corpora are measured
# with the targets that touch the most encoder code per byte, and the two
# corpora carrying an extra leading byte with the targets that read it.
target_dir=${TARGET_DIR:-target/release}
generic_target=$target_dir/q1_roundtrip
params_target=$target_dir/params_roundtrip
large_window_target=$target_dir/large_window
dictionary_target=$target_dir/dictionary

for target in "$generic_target" "$params_target" "$large_window_target" \
    "$dictionary_target"; do
    if [ ! -x "$target" ]; then
        echo "missing $target" >&2
        echo "run: cargo afl build --release" >&2
        exit 1
    fi
done

minimise() {
    corpus=$1
    target=$2

    if [ ! -d "seeds/$corpus" ]; then
        echo "missing seeds/$corpus, run prepare-seeds.sh first" >&2
        exit 1
    fi

    # Keep the unminimised corpus rather than overwriting it.
    rm -rf "seeds/$corpus.raw"
    mv "seeds/$corpus" "seeds/$corpus.raw"

    before=$(ls "seeds/$corpus.raw" | wc -l | tr -d ' ')
    cargo afl cmin -i "seeds/$corpus.raw" -o "seeds/$corpus" -- "$target"
    after=$(ls "seeds/$corpus" | wc -l | tr -d ' ')
    echo "$corpus: $before -> $after seeds (original kept in seeds/$corpus.raw)"
}

minimise generic "$generic_target"
minimise params "$params_target"
minimise large_window "$large_window_target"
minimise dictionary "$dictionary_target"
