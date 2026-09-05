# google-brotli-ffi

Rust FFI bindings to the vendored Google Brotli C library. This workspace crate
provides the encoder/decoder oracle and benchmark baseline for `mbrotli`; it is
a development dependency and is not part of the public compressor API.

## Build

The `vendor/brotli` git submodule is pinned to v1.2.0, commit `028fb5a`.
Initialize it from the repository root:

```sh
git submodule update --init --recursive
```

`build.rs` uses `cc` to compile and statically link `brotlicommon`, `brotlidec`,
`brotlienc`, and the test shim. It requires a C compiler and passes no explicit
architecture-specific optimization flags. `shim/` exposes encoder internals for
differential tests. The `experimental` feature defines `BROTLI_EXPERIMENTAL` for
serialized dictionary and custom static dictionary checks.

## Interface

The bindings mirror the upstream constants and C encoder/decoder functions.
Calling the FFI requires the safety invariants of the corresponding C API.
Tests use matching streaming parameters for byte comparisons and the C decoder
for content round trips. Native C one-shot output and arbitrary streaming
schedules are not interchangeable with the Rust serial byte-identity contract;
see [serial output identity](../architecture/universal-encoding.md).

The crate and vendored source use the MIT license; see
[the upstream license](vendor/brotli/LICENSE).
