#!/bin/sh
# Runs one AFL++ instance per fuzz target, for both feature configurations.
#
# Phase 1 fuzzes the 21 stable targets from a --no-default-features build.
# Phase 2 fuzzes all 23 targets, the stable ones included, from a
# --features experimental build: the feature reaches into the encoder, so the
# two builds run different code even for the shared targets. Every worker in
# a phase runs in parallel for the same fixed duration, from that target's
# seed corpus, into its own output directory; the two builds live in separate
# target directories so the shared binaries do not overwrite each other.
#
# Run prepare-seeds.sh and minimise-seeds.sh first. Both builds are produced
# here if they are missing. The findings root must not exist yet; AFL will
# not reuse it and the phases would otherwise mix. A DONE file is written into
# the findings root when both phases have ended.
#
# Usage: fuzz/afl/campaign.sh <findings-root> <seconds-per-phase> [timeout-ms]
#
# The six-hour campaign recorded in docs/correctness.md was
#   ./campaign.sh findings/campaign-2026-09-07-6h 10800 10000
# Its three saved hangs were quality 11 mutations of a 128 KiB seed run
# through three backends by simd_equivalence: 7.4 seconds standalone under
# instrumentation, so the default timeout is now three times that.

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

root=$1
seconds=$2
timeout_ms=${3:-30000}

if [ -e "$root" ]; then
    echo "$root already exists; choose a fresh findings root" >&2
    exit 1
fi
mkdir -p "$root"

# The WSL2 and container hosts this runs on have no CPU governor to pin, an
# external core dump handler, and no benefit from CPU binding once every
# worker is already busy. The UI is disabled because output goes to a log.
export AFL_SKIP_CPUFREQ=1 AFL_NO_AFFINITY=1 AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1 AFL_NO_UI=1

stable="q0_roundtrip q1_roundtrip q3_roundtrip q4_roundtrip q5_roundtrip q6_roundtrip \
q7_roundtrip q8_roundtrip q9_roundtrip q10_roundtrip q11_roundtrip params_roundtrip \
simd_equivalence differential_c streaming_equivalence output_capacity parameter_parsing \
large_window dictionary compressor_lifecycle parallel"
experimental="$stable serialized_dictionary framing"

seeds_for() {
    case "$1" in
        q*_roundtrip) echo seeds/generic ;;
        large_window) echo seeds/large_window ;;
        dictionary) echo seeds/dictionary ;;
        serialized_dictionary) echo seeds/serialized ;;
        compressor_lifecycle|framing|parallel) echo "regressions/$1" ;;
        *) echo seeds/params ;;
    esac
}

build() {
    config=$1
    features=$2
    if [ ! -x "target/$config/release/differential_c" ]; then
        cargo afl build --release --no-default-features $features --target-dir "target/$config"
    fi
}

phase() {
    config=$1
    targets=$2
    out="$root/$config"
    mkdir -p "$out"
    echo "== phase $config start $(date -u +%FT%TZ)"
    for target in $targets; do
        # A fixed timeout: with a trailing `+` AFL++ would calibrate its own
        # timeout from the seeds and treat the value only as a ceiling, which
        # discards the slow quality 10 and 11 mutations as timeouts. Anything
        # slower than the default on a 128 KiB payload is worth a look.
        cargo afl fuzz -i "$(seeds_for "$target")" -o "$out/$target" -V "$seconds" \
            -t "$timeout_ms" -m none -- "target/$config/release/$target" > "$out/$target.log" 2>&1 &
    done
    wait
    echo "== phase $config end $(date -u +%FT%TZ)"
}

build stable ""
build experimental "--features experimental"

phase stable "$stable"
phase experimental "$experimental"
touch "$root/DONE"
