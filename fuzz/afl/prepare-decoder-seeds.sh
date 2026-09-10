#!/bin/sh
# Builds seeds/decoder, the corpus for the targets that read a compressed
# member directly: decompress, decode_streaming and decode_io_limits.
#
# Those targets consume arbitrary bytes, so they need no seeds to run at all,
# but starting from bytes that already decode reaches the grammar far sooner
# than mutating toward a valid header from random data. Two sources feed it:
#
#   regressions/<target>/    the committed decoder inputs, which include the
#                            empty member, invalid padding, the continuation
#                            fixtures and the synthetic arbitrary-byte seeds
#   Google Brotli's testdata the reference implementation's own *.compressed*
#                            fixtures, which cover long back-references, stored
#                            members, metadata and the empty-member family
#
# Fixtures whose compressed form is above the cap are skipped: the targets bound
# a decode at 64 KiB of output, so a larger member only reaches the budget
# refusal, and a large seed costs the fuzzer far more than it teaches.
#
# Usage: fuzz/afl/prepare-decoder-seeds.sh

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

testdata="../../brotli-ffi/vendor/brotli/tests/testdata"
decoder="seeds/decoder"
max_bytes=51200

if [ ! -d "$testdata" ]; then
    echo "missing $testdata" >&2
    echo "run: git submodule update --init --recursive" >&2
    exit 1
fi

rm -rf "$decoder"
mkdir -p "$decoder"

for target in decompress decode_streaming decode_io_limits; do
    for path in "regressions/$target"/*; do
        [ -f "$path" ] || continue
        name=$(basename "$path")
        # One name per input: the three corpora share several fixtures.
        [ -e "$decoder/reg-$name" ] || cp "$path" "$decoder/reg-$name"
    done
done

for path in "$testdata"/*.compressed*; do
    [ -f "$path" ] || continue
    size=$(wc -c < "$path" | tr -d ' ')
    [ "$size" -le "$max_bytes" ] || continue
    cp "$path" "$decoder/vendor-$(basename "$path")"
done

echo "prepared $(ls "$decoder" | wc -l | tr -d ' ') decoder seeds in $decoder"
