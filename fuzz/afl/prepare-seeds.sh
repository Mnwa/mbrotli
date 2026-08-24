#!/bin/sh
# Builds the AFL seed corpora from Google Brotli's own test data.
#
# The vendored submodule at brotli-ffi/vendor/brotli ships the corpus the
# reference implementation tests against: Canterbury text, binary blobs,
# already-compressed payloads, long zero runs and back-reference edge cases.
# Reusing it keeps the seeds identical to what upstream considers interesting
# instead of duplicating a hand-rolled corpus in this repository.
#
# Three corpora are produced:
#
#   seeds/generic       the raw test data, for targets that fuzz the payload
#                       only
#   seeds/params        the same files behind a six byte parameter header, for
#                       targets that decode quality, window size, chunk size,
#                       mode, block size and distance layout from the start of
#                       the input
#   seeds/large_window  the parameter seeds behind one more byte, the RFC 9841
#                       large window the large_window target reads first
#
# Usage: fuzz/afl/prepare-seeds.sh

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
testdata="$here/../../brotli-ffi/vendor/brotli/tests/testdata"
generic="$here/seeds/generic"
params="$here/seeds/params"
large_window="$here/seeds/large_window"

# AFL refuses seeds above its default input cap, and huge seeds slow the
# fuzzer down far more than they help coverage.
max_bytes=1048576

if [ ! -d "$testdata" ]; then
    echo "missing $testdata" >&2
    echo "run: git submodule update --init --recursive" >&2
    exit 1
fi

rm -rf "$generic" "$params" "$large_window"
mkdir -p "$generic" "$params" "$large_window"

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

# Parameter headers, one per field: byte 0 the quality (index into the five
# implemented ones), byte 1 the window size (10 + value % 15), byte 2 the chunk
# size (1 << (value % 18)), byte 3 the mode and the context modelling flag,
# byte 4 the block size, byte 5 the distance layout.
#
# Small seeds get every combination; large ones get a representative few, so
# the corpus stays small enough for the fuzzer to cycle through quickly.
small_bytes=8192
full_headers="0:0:0:0:0:0 1:8:12:0:0:0 2:12:17:0:0:0 2:14:0:1:18:5 \
3:0:0:0:0:0 3:12:12:4:0:9 4:8:12:2:20:0 4:14:17:0:0:0"
large_headers="1:12:12:0:0:0 3:12:12:0:0:0 4:12:12:0:0:0"

for path in "$generic"/*; do
    name=$(basename "$path")
    size=$(wc -c < "$path" | tr -d ' ')
    if [ "$size" -le "$small_bytes" ]; then
        headers="$full_headers"
    else
        headers="$large_headers"
    fi
    for header in $headers; do
        # Split "q:w:c:f:b:d" into its six fields.
        quality=$(echo "$header" | cut -d: -f1)
        lgwin=$(echo "$header" | cut -d: -f2)
        chunk=$(echo "$header" | cut -d: -f3)
        flags=$(echo "$header" | cut -d: -f4)
        lgblock=$(echo "$header" | cut -d: -f5)
        layout=$(echo "$header" | cut -d: -f6)
        target="$params/q$quality-w$lgwin-c$chunk-f$flags-b$lgblock-d$layout-$name"
        printf "$(printf '\\%03o\\%03o\\%03o\\%03o\\%03o\\%03o' \
            "$quality" "$lgwin" "$chunk" "$flags" "$lgblock" "$layout")" > "$target"
        cat "$path" >> "$target"
    done
done

# The large window target reads one byte before the parameter header. Seed it
# with the floor, the widest window the pinned C decoder reads, the widest the
# format allows, and the ordinary default, so a campaign starts with both sides
# of the decoder's limit already covered.
for path in "$params"/*; do
    name=$(basename "$path")
    for window in 10 22 30 62; do
        target="$large_window/lw$window-$name"
        printf "$(printf '\\%03o' "$window")" > "$target"
        cat "$path" >> "$target"
    done
done

echo "prepared $count generic seeds in $generic"
echo "prepared $(ls "$params" | wc -l | tr -d ' ') parameter seeds in $params"
echo "prepared $(ls "$large_window" | wc -l | tr -d ' ') large window seeds in $large_window"
