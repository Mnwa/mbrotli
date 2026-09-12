#!/usr/bin/env python3
"""Check isolated codec consumers, including imports that must be unavailable."""
from itertools import product
from pathlib import Path
import json
import subprocess
import tomllib

root = Path(__file__).resolve().parents[1]
consumer = root / "target" / "codec-feature-consumer"
(consumer / "src").mkdir(parents=True, exist_ok=True)
defaults = tomllib.loads((root / "Cargo.toml").read_text())["features"]["default"]
assert {"compression", "decompression"} <= set(defaults), defaults

for mode, compression, decompression, experimental in product(
    ["", "std", "no_std"], [False, True], [False, True], [False, True]
):
    if not mode and (compression or decompression):
        continue  # Active codecs need a math provider; empty builds need neither.
    features = [mode] if mode else []
    features += [name for name, enabled in [
        ("compression", compression), ("decompression", decompression),
        ("experimental", experimental),
    ] if enabled]
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "codec-feature-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies.mbrotli]
path = {json.dumps(str(root))}
default-features = false
features = {json.dumps(features)}
''')
    probes = {
        "": True,
        "Compressor": compression,
        "compressor": compression,
        "EncoderSession": compression,
        "dictionary::PreparedDictionary": compression,
        "dictionary::SerializedDictionary": compression and experimental,
        "Decompressor": decompression,
        "decompressor": decompression,
        "DecoderSession": decompression,
        "dictionary::DecodeDictionary": decompression,
        "dictionary::DictionaryRef": decompression,
        "dictionary::DictionaryRef::Prepared": compression and decompression,
        "compressor::dictionary::DecodeDictionary": compression and decompression,
        "Backend": compression or decompression,
        "RetentionPolicy": compression or decompression,
        "Window": compression or decompression,
        "WindowEncoding": compression or decompression,
        "ConfigError": compression or decompression,
        "ConfigError::Quality": compression,
        "dictionary": compression or decompression,
        "io": mode == "std" and (compression or decompression),
        "io::EncoderWriter": mode == "std" and compression,
        "io::DecoderWriter": mode == "std" and decompression,
        "io::FinishError": mode == "std" and (compression or decompression),
        "compressor::parallel": mode == "std" and compression,
        "framing": (compression or decompression) and experimental,
        "framing::FramedDecompressor": decompression and experimental,
        "framing::FramedReader": mode == "std" and decompression and experimental,
        "framing::FramingConfig": compression and experimental,
        "framing::FramedCompressor": compression and experimental,
        "framing::FramedEncoderSession": compression and experimental,
        "framing::FramedInput": compression and experimental,
        "framing::FramedResourceSession": compression and experimental,
        "framing::FramedWriter": mode == "std" and compression and experimental,
        "framing::FramedEncoderReader": mode == "std" and compression and experimental,
        "compressor::framing::DictionaryId": compression and experimental,
    }
    for item, expected in probes.items():
        source = "#![no_std]\n" if mode == "no_std" else ""
        if item:
            source += f"pub use mbrotli::{item};\n"
        (consumer / "src" / "lib.rs").write_text(source)
        result = subprocess.run(
            ["cargo", "check", "--offline", "--lib", "--manifest-path", str(consumer / "Cargo.toml")],
            cwd=root, text=True, capture_output=True,
        )
        assert (result.returncode == 0) == expected, (
            f"{features}: {item or 'empty consumer'}, expected={expected}\n{result.stderr}"
        )
        if not expected:
            assert "error[E0432]" in result.stderr or "error[E0433]" in result.stderr, result.stderr
    print(f"{','.join(features) or 'no features'}: codec API gates OK", flush=True)
