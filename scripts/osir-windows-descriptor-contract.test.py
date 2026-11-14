"""Portable gate regressions. None of these constitute native Windows proof."""
from __future__ import annotations

import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


producer = load('producer_fixture_test', ROOT / 'scripts/osir-windows-descriptor-producer.py')
gate = load('descriptor_gate_test', ROOT / 'scripts/osir-windows-descriptor-contract.py')
SAMPLE = b'use ignored;\n#[cfg(not(unix))]\nfn write_binding(directory: &Path, bytes: &[u8]) -> Result<(), &\'static str> { Ok(()) }\n\n#[cfg(test)]\nmod tests;\n'


class FixtureTests(unittest.TestCase):
    def test_extracts_exact_single_frozen_function_without_surrounding_source(self):
        for raw in (SAMPLE, SAMPLE.replace(b'\n', b'\r\n')):
            extracted = producer.extract_writer(raw)
            self.assertIn(extracted, raw)
            self.assertNotIn(b'use ignored', extracted)
            self.assertNotIn(b'mod tests', extracted)
            self.assertEqual(extracted.count(b'fn write_binding('), 1)
        for raw in (SAMPLE + SAMPLE, b'fn write_binding() {}', SAMPLE.replace(b'#[cfg(test)]', b'// absent'),
                    SAMPLE.replace(b'Ok(())', b'fn hidden() {} Ok(())')):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                producer.extract_writer(raw)

    def test_hash_binding_exclusive_output_and_exact_function_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / 'source.rs'
            source.write_bytes(SAMPLE)
            with self.assertRaises(ValueError):
                producer.prepare(source, '0' * 64, root / 'must-not-exist')
            self.assertFalse((root / 'must-not-exist').exists())
            digest = hashlib.sha256(SAMPLE).hexdigest()
            receipt = producer.prepare(source, digest, root / 'adapter')
            generated = (root / 'adapter/src/main.rs').read_bytes()
            self.assertIn(producer.extract_writer(SAMPLE), generated)
            self.assertEqual(receipt['clientSourceSha256'], digest)
            self.assertTrue(receipt['fullClientPublicationNotTested'])
            self.assertIn('tempfile = "=3.27.0"', (root / 'adapter/Cargo.toml').read_text())
            with self.assertRaises(FileExistsError):
                producer.prepare(source, digest, root / 'adapter')

    def test_required_native_checks_are_explicit_and_do_not_omit_real_producer(self):
        self.assertEqual(len(gate.REQUIRED_CHECKS), 18)
        self.assertEqual(len(set(gate.REQUIRED_CHECKS)), 18)
        for name in ['defaultProfileRealClientProducerAccepted', 'profileAncestorSecurityUnchanged',
                     'nonPrivateConfigRetainsOwnerPolicy', 'foreignOwnerRejected', 'heldLeafAndAncestorDenyReplacement']:
            self.assertIn(name, gate.REQUIRED_CHECKS)

    @unittest.skipIf(sys.platform == 'win32', 'This specifically checks non-Windows refusal')
    def test_non_windows_gate_is_failed_unsupported_never_native_passed(self):
        reader = ROOT / 'connector/connector/runtimes/codex/osir_binding_windows.py'
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / 'evidence'
            result = subprocess.run([sys.executable, str(ROOT / 'scripts/osir-windows-descriptor-contract.py'),
                '--reader', str(reader), '--reader-sha256', gate.sha(reader), '--source-commit', '1' * 40,
                '--client-source-sha256', '2' * 64, '--producer', str(Path(tmp) / 'never-executed'),
                '--producer-sha256', '3' * 64, '--producer-receipt', str(Path(tmp) / 'never-read'),
                '--output', str(output)], capture_output=True, check=False)
            self.assertEqual(result.returncode, 1, result.stderr)
            report = json.loads((output / 'report.json').read_text())
            self.assertEqual(report['result'], 'failed')
            self.assertEqual(report['evidenceMode'], 'unsupported')
            self.assertEqual(report['nativeMachines'], [])
            self.assertEqual(report['checks'], {})
            item = report['evidence'][0]
            self.assertEqual(item['sha256'], gate.sha(output / item['path']))


if __name__ == '__main__':
    unittest.main()
