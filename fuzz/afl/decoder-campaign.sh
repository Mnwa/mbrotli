#!/bin/sh
# Runs one AFL++ instance per decoder target, for both feature configurations.
#
# campaign.sh fuzzes the encoder targets and decode_roundtrip. This script
# fuzzes the decoder surface instead: the six base targets and, in the
# experimental build, decode_serialized. Both phases run at once, thirteen
# workers in total, which fits a host with sixteen or more hardware threads
# without oversubscription. Every worker runs for the same fixed duration, from
# its own seed corpus, into its own output directory; the two builds live in
# separate target directories so the shared binaries do not overwrite each
# other.
#
# The stream targets read a compressed member directly, so their corpus is
# whatever prepare-decoder-seeds.sh materialised: the committed regression
# inputs plus Google Brotli's own compressed fixtures. decode_roundtrip reads
# the encoder's six-byte parameter header, so it uses seeds/params. The
# remaining targets carry their own input models and use their committed
# regression corpora.
#
# The findings root must not exist yet; AFL will not reuse it. A DONE file is
# written there when both phases have ended.
#
# Usage: [SEED_ROOT=<dir>] fuzz/afl/decoder-campaign.sh <findings-root> \
#            <seconds> [timeout-ms]
#
# The three-hour campaign recorded in docs/correctness.md was
#   ./prepare-decoder-seeds.sh
#   SEED_ROOT=seeds/decoder-cmin \
#       ./decoder-campaign.sh findings/decoder-2026-09-10-3h-fixed 10800
# where seeds/decoder-cmin held the `cargo afl cmin` reduction of the queues an
# earlier, aborted campaign had grown from the default corpora.
#
# The timeout is fixed rather than calibrated: with a trailing `+` AFL++ would
# derive its own ceiling from the seeds and discard the slower decodes as
# timeouts. Five seconds is far above any decode of a 128 KiB member, so
# anything that reaches it is worth a look.

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

root=$1
seconds=$2
timeout_ms=${3:-5000}

if [ -e "$root" ]; then
    echo "$root already exists; choose a fresh findings root" >&2
    exit 1
fi
mkdir -p "$root"

# The WSL2 and container hosts this runs on have no CPU governor to pin, an
# external core dump handler, and no benefit from CPU binding once every
# worker is already busy. The UI is disabled because output goes to a log.
export AFL_SKIP_CPUFREQ=1 AFL_NO_AFFINITY=1 AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1 AFL_NO_UI=1

base="decompress decode_streaming decode_roundtrip decode_dictionary \
decode_lifecycle decode_io_limits"
experimental="$base decode_serialized"

# Set SEED_ROOT to carry a previous campaign's exploration forward: when
# "$SEED_ROOT/<config>-<target>" exists it replaces the default corpus for that
# worker. A queue minimised with `cargo afl cmin` belongs there, not a raw one.
seeds_for() {
    if [ -n "${SEED_ROOT:-}" ] && [ -d "$SEED_ROOT/$1-$2" ]; then
        echo "$SEED_ROOT/$1-$2"
        return
    fi
    case "$2" in
        decompress | decode_streaming | decode_io_limits) echo seeds/decoder ;;
        decode_roundtrip) echo seeds/params ;;
        *) echo "regressions/$2" ;;
    esac
}

build() {
    config=$1
    features=$2
    if [ ! -x "target/$config/release/decompress" ]; then
        cargo afl build --release --no-default-features $features \
            --target-dir "target/$config"
    fi
}

phase() {
    config=$1
    targets=$2
    out="$root/$config"
    mkdir -p "$out"
    echo "== phase $config start $(date -u +%FT%TZ)"
    for target in $targets; do
        seeds=$(seeds_for "$config" "$target")
        if [ ! -d "$seeds" ]; then
            echo "missing $seeds; run prepare-decoder-seeds.sh and prepare-seeds.sh" >&2
            exit 1
        fi
        cargo afl fuzz -i "$seeds" -o "$out/$target" -V "$seconds" \
            -t "$timeout_ms" -m none -- "target/$config/release/$target" \
            > "$out/$target.log" 2>&1 &
    done
    # Without this the phase subshell would return while its workers are still
    # running, and the caller's `wait` would report the campaign finished at
    # once.
    wait
    echo "== phase $config end $(date -u +%FT%TZ)"
}

build stable ""
build experimental "--features experimental"

phase stable "$base" &
phase experimental "$experimental" &
wait
echo "== campaign end $(date -u +%FT%TZ)"
touch "$root/DONE"
