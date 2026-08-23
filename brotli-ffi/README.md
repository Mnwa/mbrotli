# google-brotli-ffi

Raw Rust FFI bindings to Google's Brotli C library, used by [`mbrotli`](../)
as its differential-test oracle and benchmark baseline.

The C sources are vendored as a git submodule at `vendor/brotli`, pinned to
**v1.2.0, commit `028fb5a`**. That tree is upstream source: it is not
hand-edited, and only an explicit upstream-update change should touch it.

```sh
git submodule update --init --recursive
```

`build.rs` compiles `brotlicommon`, `brotlidec` and `brotlienc` with the `cc`
crate and links them statically. No architecture-specific flags are passed, so
the result is a portable baseline build — which is what makes it a fair
benchmark counterpart to the Rust encoder.

The crate exposes the encoder and decoder C API verbatim: every item is
`unsafe extern "C"`, with the upstream constants and enums mirrored as-is. It
is a development dependency of `mbrotli`, not part of its public API.

Distributed under the MIT licence, as is the vendored Brotli source; see
`vendor/brotli/LICENSE`.
