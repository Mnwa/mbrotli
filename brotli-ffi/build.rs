use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn c_sources(directory: &str) -> Vec<PathBuf> {
    let mut sources = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {directory}: {error}"))
        .map(|entry| entry.expect("failed to read vendored source entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect::<Vec<_>>();
    sources.sort();
    sources
}

/// Whether to compile the vendored sources with `BROTLI_EXPERIMENTAL`.
///
/// The reference gates its RFC 9841 serialized shared dictionary parser, its
/// custom static dictionary construction, and one `static_dict.c` search branch
/// behind that flag. None of the three is reachable from an ordinary
/// compression, so defining it does not move any output; it is still kept
/// behind a feature so the default build of this crate is the reference's own
/// default build, byte for byte.
fn experimental() -> bool {
    env::var_os("CARGO_FEATURE_EXPERIMENTAL").is_some()
}

fn compile_library(name: &str, directory: &str) {
    let sources = c_sources(directory);
    let mut build = cc::Build::new();
    build
        .include("vendor/brotli/c/include")
        .include("vendor/brotli")
        .cargo_metadata(false)
        .warnings(false);
    if experimental() {
        build.define("BROTLI_EXPERIMENTAL", None);
    }
    build.files(&sources).compile(name);

    for source in sources {
        println!("cargo:rerun-if-changed={}", source.display());
    }
}

/// Compiles the test-only shim that exposes one encoder-internal function.
///
/// It is linked unconditionally: it is a single small translation unit, and a
/// feature gate would make the test and non-test builds of this crate differ.
fn compile_shim() {
    let mut build = cc::Build::new();
    build
        .include("vendor/brotli/c/include")
        .include("vendor/brotli/c")
        .include("vendor/brotli")
        .cargo_metadata(false)
        .warnings(false);
    if experimental() {
        build.define("BROTLI_EXPERIMENTAL", None);
    }
    build
        .file("shim/static_dict_probe.c")
        .compile("mbrotli_shim");
    println!("cargo:rerun-if-changed=shim/static_dict_probe.c");
}

fn main() {
    let target_family = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let output_directory = env::var("OUT_DIR").expect("OUT_DIR is not set");

    compile_library("brotlicommon", "vendor/brotli/c/common");
    compile_library("brotlidec", "vendor/brotli/c/dec");
    compile_library("brotlienc", "vendor/brotli/c/enc");
    compile_shim();

    // Dependents precede their dependencies for single-pass static linkers.
    println!("cargo:rustc-link-search=native={output_directory}");
    println!("cargo:rustc-link-lib=static=mbrotli_shim");
    println!("cargo:rustc-link-lib=static=brotlienc");
    println!("cargo:rustc-link-lib=static=brotlidec");
    println!("cargo:rustc-link-lib=static=brotlicommon");

    // The encoder uses logarithmic functions supplied by libm on Unix-like
    // targets. Apple platforms provide them through the system library.
    if target_family == "unix" && !env::var("CARGO_CFG_TARGET_VENDOR").is_ok_and(|v| v == "apple") {
        println!("cargo:rustc-link-lib=m");
    }

    println!(
        "cargo:include={}",
        absolute("vendor/brotli/c/include").display()
    );
    println!("cargo:rerun-if-changed=vendor/brotli/c/include");
    println!("cargo:rerun-if-changed=UPSTREAM_VERSION");
}

fn absolute(path: impl AsRef<Path>) -> PathBuf {
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"))
        .join(path)
}
