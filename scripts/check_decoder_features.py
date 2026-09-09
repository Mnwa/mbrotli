#!/usr/bin/env python3
"""Compile isolated production consumers; verify base API and custom API gates."""
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[1]
consumer = root / "target" / "decoder-feature-consumer"
(consumer / "src").mkdir(parents=True, exist_ok=True)
base = '''use mbrotli::{Decompressor, DecoderConfig};
use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment, PreparedDictionary};
pub fn base(prepared: &PreparedDictionary) {
    let dictionary = DecodeDictionary::new(&[DictionaryAttachment::Raw(b"prefix")], DecodeDictionaryLimits::default()).unwrap();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    let _ = decoder.decompress_with_dictionary(&dictionary, b"");
    let _ = decoder.decompress_with_dictionary(prepared, b"");
}
'''
custom = '''pub fn custom(bytes: &[u8]) {
    let _ = DictionaryAttachment::Serialized(bytes);
    let _ = core::mem::size_of::<mbrotli::dictionary::SerializedDictionary>();
}
'''
for name, features in [("base-std", ["std"]), ("experimental-std", ["std", "experimental"]), ("base-alloc", ["no_std"]), ("experimental-alloc", ["no_std", "experimental"])]:
    features += ["compression", "decompression"]
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "decoder-feature-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies.mbrotli]
path = "{root.as_posix()}"
default-features = false
features = {features!r}
'''.replace("'", '"'))
    for extended in [False, True]:
        (consumer / "src" / "lib.rs").write_text(("#![no_std]\n" if "alloc" in name else "") + base + (custom if extended else ""))
        result = subprocess.run(["cargo", "check", "--offline", "--lib", "--manifest-path", str(consumer / "Cargo.toml")], cwd=root, text=True, capture_output=True)
        expected = not extended or "experimental" in features
        assert (result.returncode == 0) == expected, f"{name}, custom={extended}\n{result.stderr}"
        if not expected:
            assert "Serialized" in result.stderr and "SerializedDictionary" in result.stderr, result.stderr
        (consumer / f"{name}-{'custom' if extended else 'base'}.log").write_text(result.stdout + result.stderr)
        print(f"{name}: {'custom gate' if extended else 'base API'} OK ({'compiles' if expected else 'rejected'})", flush=True)
