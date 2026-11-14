"""Build a test-only adapter around the exact frozen Client Windows writer.

Only the published write_binding function is extracted byte-for-byte. No copy
is edited, no full Client build is performed, and no real Client data is read.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

START = '#[cfg(not(unix))]\nfn write_binding('
END = '\n#[cfg(test)]\n'


def extract_writer(source: bytes) -> bytes:
    # CRLF belongs to the source identity; normalize only to find line offsets.
    text = source.decode('utf-8')
    marker = START.replace('\n', '\r\n') if '\r\n' in text else START
    end = END.replace('\n', '\r\n') if '\r\n' in text else END
    if text.count(marker) != 1:
        raise ValueError('Expected exactly one frozen Windows writer')
    first = text.index(marker)
    last = text.find(end, first)
    if last < 0:
        raise ValueError('Missing writer end boundary')
    function = text[first:last].encode('utf-8')
    # A revision that changes this boundary must be explicitly reviewed.
    if not function.rstrip().endswith(b'}') or b'fn ' in function.split(b'fn write_binding(', 1)[1]:
        raise ValueError('Unexpected code in writer extraction')
    return function


def prepare(source: Path, expected: str, destination: Path) -> dict:
    if not re.fullmatch(r'[0-9a-f]{64}', expected):
        raise ValueError('An exact lowercase source SHA256 is required')
    raw = source.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    if digest != expected:
        raise ValueError('Client writer source identity mismatch')
    writer = extract_writer(raw)
    destination.mkdir(parents=True, exist_ok=False)
    (destination / 'src').mkdir()
    (destination / 'Cargo.toml').write_text('''[package]
name = "osir-windows-real-writer-fixture"
version = "0.0.0"
edition = "2021"
[dependencies]
tempfile = "=3.27.0"
[profile.dev]
debug = 0
incremental = false
''', encoding='utf-8')
    # Input directory must already be an exclusively-created synthetic subtree
    # directly under USERPROFILE. The harness passes only a synthetic payload.
    prefix = b'use std::{fs, path::{Path, PathBuf}};\n'
    main = br'''
fn main() {
    let mut args = std::env::args_os().skip(1);
    let directory = PathBuf::from(args.next().expect("synthetic directory required"));
    assert!(args.next().is_none(), "one argument required");
    let profile = PathBuf::from(std::env::var_os("USERPROFILE").expect("profile required"));
    assert!(directory.is_absolute() && directory.parent() == Some(profile.as_path()));
    assert!(directory.file_name().unwrap().to_string_lossy().starts_with("osir-reader-contract-"));
    assert!(directory.is_dir() && !directory.join("codex-binding.json").exists());
    write_binding(&directory, b"{\"schemaVersion\":1,\"source\":\"osir-client\",\"available\":false,\"errorCode\":\"synthetic-contract\"}")
        .expect("actual Client writer failed");
}
'''
    (destination / 'src/main.rs').write_bytes(prefix + writer + b'\n' + main)
    receipt = {'schemaVersion': 1, 'kind': 'test-only-exact-client-writer-adapter',
               'clientSourceSha256': digest, 'extractedFunctionSha256': hashlib.sha256(writer).hexdigest(),
               'extractedFunctionUnchanged': True, 'tempfileVersion': '3.27.0',
               'syntheticDataOnly': True, 'fullClientPublicationNotTested': True}
    (destination / 'producer-source.json').write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--client-source', type=Path, required=True)
    parser.add_argument('--client-source-sha256', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(prepare(args.client_source, args.client_source_sha256, args.output)))


if __name__ == '__main__':
    main()
