# google-brotli-ffi

Raw Rust FFI bindings to the C implementation of Brotli maintained by Google.
The upstream C sources are linked as a Git submodule pinned to Google Brotli
v1.2.0 and built statically by this crate.

The bindings cover the public common, encoder, and decoder APIs, including the
one-shot, streaming, metadata, and shared-dictionary interfaces. All functions
are unsafe because they preserve the original C pointer-based API.

Upstream: <https://github.com/google/brotli>

## Cloning

Initialize the C source after cloning this repository:

```shell
git submodule update --init --recursive
```

## Updating the C source

Check out the desired tagged Google Brotli release in `vendor/brotli`, commit
the updated submodule pointer, and keep `UPSTREAM_VERSION` and the version
documented above in sync.
