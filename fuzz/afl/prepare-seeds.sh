#!/bin/sh
# Builds the AFL seed corpora from Google Brotli's own test data.
#
# The vendored submodule at brotli-ffi/vendor/brotli ships the corpus the
# reference implementation tests against: Canterbury text, binary blobs,
# already-compressed payloads, long zero runs and back-reference edge cases.
# Reusing it keeps the seeds identical to what upstream considers interesting
# instead of duplicating a hand-rolled corpus in this repository.
#
# Two corpora are produced:
#
#   seeds/generic  the raw test data, for targets that fuzz the payload only
#   seeds/params   the same files behind a three byte parameter header, for
#                  targets that decode quality, window size and chunk size
#                  from the start of the input
#
# Usage: fuzz/afl/prepare-seeds.sh

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
testdata="$here/../../brotli-ffi/vendor/brotli/tests/testdata"
generic="$here/seeds/generic"
params="$here/seeds/params"

# AFL refuses seeds above its default input cap, and huge seeds slow the
# fuzzer down far more than they help coverage.
max_bytes=1048576

if [ ! -d "$testdata" ]; then
    echo "missing $testdata" >&2
    echo "run: git submodule update --init --recursive" >&2
    exit 1
fi

rm -rf "$generic" "$params"
mkdir -p "$generic" "$params"

count=0
for path in "$testdata"/*; do
    name=$(basename "$path")
    # .compressed files are decoder fixtures, not encoder inputs.
    case "$name" in
        *.compressed*) continue ;;
    esac
    [ -f "$path" ] || continue

    size=$(wc -c < "$path" | tr -d ' ')
    if [ "$size" -gt "$max_bytes" ]; then
        # Keep a prefix of the oversized binaries; they still carry the
        # structure that makes them interesting.
        dd if="$path" of="$generic/$name" bs="$max_bytes" count=1 2>/dev/null
    else
        cp "$path" "$generic/$name"
    fi
    count=$((count + 1))
done

# Parameter headers: quality 0/1, a spread of window sizes, a spread of
# streaming chunk sizes. Byte 0 picks the quality, byte 1 the window size
# (10 + value % 15), byte 2 the chunk size (1 << (value % 18)).
#
# Small seeds get every combination; large ones get a representative few, so
# the corpus stays small enough for the fuzzer to cycle through quickly.
small_bytes=8192
full_headers="0:0:0 0:8:12 0:12:17 0:14:0 1:0:0 1:8:12 1:12:17 1:14:0"
large_headers="0:12:12 1:12:12"

for path in "$generic"/*; do
    name=$(basename "$path")
    size=$(wc -c < "$path" | tr -d ' ')
    if [ "$size" -le "$small_bytes" ]; then
        headers="$full_headers"
    else
        headers="$large_headers"
    fi
    for header in $headers; do
        quality=${header%%:*}
        rest=${header#*:}
        lgwin=${rest%%:*}
        chunk=${rest##*:}
        target="$params/q$quality-w$lgwin-c$chunk-$name"
        printf "$(printf '\\%03o\\%03o\\%03o' "$quality" "$lgwin" "$chunk")" > "$target"
        cat "$path" >> "$target"
    done
done

echo "prepared $count generic seeds in $generic"
echo "prepared $(ls "$params" | wc -l | tr -d ' ') parameter seeds in $params"
