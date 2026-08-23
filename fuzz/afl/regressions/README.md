# Regression corpus

One directory per fuzz target, named after its binary. Every `.bin` file in
`<target>/` is replayed through `mbrotli_afl::targets::<target>` by
`tests/regressions.rs`, so these inputs are checked by `cargo afl test` and
never depend on a running fuzzer.

Two kinds of file live here:

- `boundary-*.bin` — hand-written edge cases committed up front: empty input,
  truncated and extreme parameter headers, minimum and maximum window sizes,
  the smallest and largest streaming chunk sizes, incompressible payloads.
- `crash-*.bin` — minimised reproducers for confirmed findings. Add one for
  every crash **before** fixing the bug it exposes.

## Adding a finding

```sh
cd fuzz/afl
cargo afl tmin -i findings/<campaign>/default/crashes/id:000000,... \
    -o regressions/<target>/crash-<short-description>.bin \
    -- target/release/<target>
cargo afl test
```

The last command must fail on the new input. Fix the bug, then run it again;
it must pass. Keep the inputs small — `tmin` output, not raw crash files — and
review them before committing, since they are derived from the vendored Brotli
test data.
