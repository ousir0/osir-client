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
from types import SimpleNamespace

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
        self.assertEqual(len(gate.REQUIRED_CHECKS), 20)
        self.assertEqual(len(set(gate.REQUIRED_CHECKS)), 20)
        for name in ['defaultProfileRealClientProducerAccepted', 'profileAncestorSecurityUnchanged',
                     'nonPrivateConfigRetainsOwnerPolicy', 'foreignOwnerRejected', 'heldLeafAndAncestorDenyReplacement',
                     'effectiveTokenOwnerObserved', 'denyOnlyPrivilegedOwnerRejected']:
            self.assertIn(name, gate.REQUIRED_CHECKS)

    def test_testonly_impersonation_reverts_and_closes_handles_even_on_read_error(self):
        # Synthetic fixture lifecycle test; no Windows or account is modified.
        reader = gate.load_reader(ROOT / 'connector/connector/runtimes/codex/osir_binding_windows.py')
        for deny_admin in [False, True]:
            state = {'impersonating': False, 'closed': [], 'defaultChanges': [], 'disableFlags': []}
            fixture = gate.NativeFixtures.__new__(gate.NativeFixtures)
            fixture.reader, fixture.user = reader, reader._well_known(5, 21, 1000)
            def identity():
                return reader._TokenIdentity(fixture.user, fixture.user, (), (), 'thread' if state['impersonating'] else 'process')
            def set_handle(out, value):
                out._obj.value = value
                return True
            def default_owner(_token, kind, _owner, _size):
                state['defaultChanges'].append(kind)
                return True
            def disable(_token, flags, count, _sids, _privilege_count, _privileges, _restrict_count, _restrict, out):
                state['disableFlags'].append((flags, count))
                return set_handle(out, 303)
            def impersonate(_thread, _token):
                state['impersonating'] = True
                return True
            def revert():
                state['impersonating'] = False
                return True
            fixture.api = SimpleNamespace(identity=identity, checked=lambda value, _reason: self.assertTrue(value),
                close=lambda handle: state['closed'].append(handle.value))
            fixture.kernel = SimpleNamespace(GetCurrentProcess=lambda: -1)
            fixture.advapi = SimpleNamespace(OpenProcessToken=lambda _process, _access, out: set_handle(out, 101),
                DuplicateTokenEx=lambda _token, _access, _attributes, _level, _type, out: set_handle(out, 202),
                SetTokenInformation=default_owner, CreateRestrictedToken=disable, SetThreadToken=impersonate, RevertToSelf=revert)
            with self.subTest(deny_admin=deny_admin), self.assertRaisesRegex(ValueError, 'synthetic-read-failure'):
                with fixture.test_token(deny_admin=deny_admin) as actual:
                    self.assertEqual(actual.source, 'thread')
                    raise ValueError('synthetic-read-failure')
            self.assertFalse(state['impersonating'])
            self.assertEqual(state['closed'], [303, 202, 101] if deny_admin else [202, 101])
            self.assertEqual(state['defaultChanges'], [4])
            self.assertEqual(state['disableFlags'], [(0, 1)] if deny_admin else [])

    def test_rejection_mismatch_preserves_native_cause_and_never_turns_acceptance_into_pass(self):
        reader = gate.load_reader(ROOT / 'connector/connector/runtimes/codex/osir_binding_windows.py')
        def path_access_denied():
            raise reader.WindowsFileReadError('open-file', 5)
        self.assertEqual(gate.rejection(reader, path_access_denied), {'reason': 'open-file', 'winerror': 5})
        with self.assertRaises(AssertionError) as error:
            gate.rejection(reader, path_access_denied, reason='foreign-owner')
        self.assertEqual(gate.failure_observation(error.exception), {
            'reason': 'AssertionError', 'winerror': None,
            'causedBy': {'reason': 'open-file', 'winerror': 5}})
        with self.assertRaises(AssertionError):
            gate.rejection(reader, lambda: b'accepted-unsafe-file', reason='foreign-owner')

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
