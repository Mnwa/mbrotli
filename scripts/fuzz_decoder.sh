#!/bin/sh
# Reproducible bounded decoder campaigns; run after cargo afl build --release.
set -eu
cd "$(dirname "$0")/../fuzz/afl"
profile=${1:-base}
seconds=${2:-60}
case "$profile" in
  base) targets="decompress decode_streaming decode_dictionary decode_lifecycle decode_io_limits" ;;
  experimental) targets="decompress decode_streaming decode_dictionary decode_lifecycle decode_io_limits decode_serialized" ;;
  *) echo "profile must be base or experimental" >&2; exit 2 ;;
esac
export AFL_SKIP_CPUFREQ=1 AFL_NO_AFFINITY=1 AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1
for decoder_target in $targets; do
  findings="../../target/decoder-afl-$profile-$decoder_target${3:+-$3}"
  cargo afl fuzz -i "regressions/$decoder_target" -o "$findings" -V "$seconds" -t 1000 -c - -- "target/release/$decoder_target" > "$findings.log" 2>&1
  python3 - "$findings" <<'PY'
from pathlib import Path
import sys
root = Path(sys.argv[1])
failures = [p for p in root.rglob('id:*') if p.parent.name in ('crashes', 'hangs')]
assert not failures, f"AFL findings require triage: {failures}"
print(root.name + ': no saved crashes or hangs', flush=True)
PY
done
