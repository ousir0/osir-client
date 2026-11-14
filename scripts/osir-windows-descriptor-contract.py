"""Native Windows contract gate. Operates only on its own synthetic profile subtree.

This proves the reader primitive and the frozen Client write_binding function,
not packaged Client publication, Provider routing or a real model request.
"""
from __future__ import annotations

import argparse
import ctypes as C
import hashlib
import importlib.util
import json
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import tempfile
from contextlib import ExitStack
from pathlib import Path


REQUIRED_CHECKS = ('defaultProfileRealClientProducerAccepted', 'trustedWritersAndForeignReadAccepted', 'safeInheritedDaclAccepted', 'foreignMutationRightsRejected', 'inheritedForeignWriterRejected', 'nullDaclRejected', 'foreignOwnerRejected', 'directoryRejected', 'multipleLinksRejected', 'leafReparseRejected', 'ancestorJunctionRejected', 'oversizeRejected', 'nonPrivateConfigRetainsOwnerPolicy', 'activeWriterRejected', 'heldLeafAndAncestorDenyReplacement', 'handlesClosed', 'profileAncestorSecurityUnchanged', 'syntheticSubtreeRemoved')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_reader(path):
    spec = importlib.util.spec_from_file_location('osir_windows_native_reader', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class NativeFixtures:
    """Fixture-only mutations. Production reader never calls these APIs."""
    def __init__(self, reader):
        self.reader = reader
        self.api = reader._api()
        self.user = self.api.user()
        self.kernel, self.advapi = self.api.kernel, self.api.advapi
        bind = self.api._bind
        bind(self.advapi, 'ConvertStringSecurityDescriptorToSecurityDescriptorW',
             [C.c_wchar_p, reader._DWORD, C.POINTER(reader._HANDLE), C.POINTER(reader._DWORD)], reader._BOOL)
        bind(self.advapi, 'SetFileSecurityW', [C.c_wchar_p, reader._DWORD, reader._HANDLE], reader._BOOL)
        bind(self.kernel, 'GetProcessHandleCount', [reader._HANDLE, C.POINTER(reader._DWORD)], reader._BOOL)
        bind(self.kernel, 'CreateSymbolicLinkW', [C.c_wchar_p, C.c_wchar_p, reader._DWORD], C.c_ubyte)
        bind(self.kernel, 'DeviceIoControl', [reader._HANDLE, reader._DWORD, reader._HANDLE, reader._DWORD,
             reader._HANDLE, reader._DWORD, C.POINTER(reader._DWORD), reader._HANDLE], reader._BOOL)

    @staticmethod
    def sid_text(raw):
        return 'S-1-' + str(int.from_bytes(raw[2:8], 'big')) + ''.join('-' + str(n) for n in struct.unpack('<' + 'I' * raw[1], raw[8:]))

    def set_security(self, path, dacl, *, owner=None):
        value = ('O:' + owner if owner else '') + 'D:' + dacl
        descriptor = self.reader._HANDLE()
        self.api.checked(self.advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(value, 1, C.byref(descriptor), None), 'fixture-sddl')
        try:
            # Fixtures may protect THEIR OWN synthetic file, never ancestors.
            flags = 4 | (0x80000000 if dacl.startswith('P') else 0) | (1 if owner else 0)
            self.api.checked(self.advapi.SetFileSecurityW(str(path), flags, descriptor), 'fixture-set-security')
        finally:
            self.kernel.LocalFree(descriptor)

    def trusted(self, path, extra=''):
        self.set_security(path, 'P(A;;FA;;;' + self.sid_text(self.user) + ')(A;;FA;;;SY)(A;;FA;;;BA)' + extra)

    def ancestry(self, profile):
        values = []
        for path in reversed((profile, *profile.parents)):
            with ExitStack() as stack:
                handle = self.kernel.CreateFileW('\\\\?\\' + str(path), 0x20080, 3, None, 3, 0x2200000, None)
                if handle in (None, C.c_void_p(-1).value):
                    self.api.checked(False, 'fixture-ancestor-open')
                stack.callback(self.api.close, handle)
                owner, dacl, descriptor = self.reader._HANDLE(), self.reader._HANDLE(), self.reader._HANDLE()
                code = self.advapi.GetSecurityInfo(handle, 1, 5, C.byref(owner), None, C.byref(dacl), None, C.byref(descriptor))
                if code:
                    raise self.reader.WindowsFileReadError('fixture-ancestor-security', code)
                try:
                    raw = None if not dacl.value else C.string_at(dacl, struct.unpack('<H', C.string_at(dacl.value + 2, 2))[0])
                    values.append({'path': str(path), 'ownerSid': self.sid_text(self.api.sid(owner)),
                                   'daclSha256': hashlib.sha256(raw).hexdigest() if raw else None})
                finally:
                    self.kernel.LocalFree(descriptor)
        return values

    def security_observation(self, path):
        with ExitStack() as stack:
            handle = self.api.open(str(path), directory=False, stack=stack)
            owner, dacl, descriptor = self.reader._HANDLE(), self.reader._HANDLE(), self.reader._HANDLE()
            code = self.advapi.GetSecurityInfo(handle, 1, 5, C.byref(owner), None, C.byref(dacl), None, C.byref(descriptor))
            if code:
                raise self.reader.WindowsFileReadError('fixture-leaf-security', code)
            try:
                raw = None if not dacl.value else C.string_at(dacl, struct.unpack('<H', C.string_at(dacl.value + 2, 2))[0])
                entries = []
                if raw:
                    offset = 8
                    for _ in range(struct.unpack_from('<H', raw, 4)[0]):
                        kind, flags, size = struct.unpack_from('<BBH', raw, offset)
                        entries.append({'kind': kind, 'flags': flags, 'mask': hex(struct.unpack_from('<I', raw, offset + 4)[0]) if size >= 8 else None})
                        offset += size
                return {'ownerSid': self.sid_text(self.api.sid(owner)), 'effectiveUserSid': self.sid_text(self.user),
                        'daclSha256': hashlib.sha256(raw).hexdigest() if raw else None, 'aces': entries}
            finally:
                self.kernel.LocalFree(descriptor)

    def handles(self):
        count = self.reader._DWORD()
        self.api.checked(self.kernel.GetProcessHandleCount(self.kernel.GetCurrentProcess(), C.byref(count)), 'fixture-handle-count')
        return count.value

    def junction(self, link, target):
        link.mkdir()
        substitute = ('\\??\\' + str(target)).encode('utf-16-le')
        printable = str(target).encode('utf-16-le')
        names = substitute + b'\x00\x00' + printable + b'\x00\x00'
        payload = struct.pack('<IHHHHHH', 0xA0000003, 8 + len(names), 0, 0, len(substitute), len(substitute) + 2, len(printable)) + names
        with ExitStack() as stack:
            handle = self.kernel.CreateFileW(str(link), 0x40000000, 0, None, 3, 0x2200000, None)
            if handle in (None, C.c_void_p(-1).value):
                self.api.checked(False, 'fixture-junction-open')
            stack.callback(self.api.close, handle)
            count, buffer = self.reader._DWORD(), C.create_string_buffer(payload)
            self.api.checked(self.kernel.DeviceIoControl(handle, 0x900A4, buffer, len(payload), None, 0, C.byref(count), None), 'fixture-create-junction')


def validate_producer(args):
    receipt = json.loads(args.producer_receipt.read_text(encoding='utf-8'))
    if (receipt.get('kind') != 'test-only-exact-client-writer-adapter'
            or receipt.get('clientSourceSha256') != args.client_source_sha256
            or receipt.get('extractedFunctionUnchanged') is not True
            or receipt.get('tempfileVersion') != '3.27.0'
            or sha(args.producer) != args.producer_sha256):
        raise ValueError('Producer binding mismatch')
    return receipt


def run_native(args, report, observations):
    if os.name != 'nt':
        raise RuntimeError('Windows native execution is required')
    reader = load_reader(args.reader)
    fixtures = NativeFixtures(reader)
    profile = Path(os.environ['USERPROFILE'])
    # Preserve drive / C:\Users / profile security exactly as supplied by runner.
    before_ancestors = fixtures.ancestry(profile)
    observations['profileAncestorsBefore'] = before_ancestors
    report['nativeMachines'] = ['windows-' + platform.machine().lower()]
    observations['abiFileInfoBytes'] = C.sizeof(reader._FileInfo)
    if C.sizeof(reader._FileInfo) != 52:
        raise ValueError('Unexpected Win32 file information ABI')
    root = Path(tempfile.mkdtemp(prefix='osir-reader-contract-', dir=profile))
    results = observations['cases'] = []

    def case(name, callback):
        try:
            detail = callback()
            report['checks'][name] = True
            results.append({'check': name, 'result': 'passed', 'detail': detail})
        except Exception as error:
            report['checks'][name] = False
            results.append({'check': name, 'result': 'failed', 'reason': getattr(error, 'reason', type(error).__name__),
                            'winerror': getattr(error, 'winerror', None)})

    def rejects(path, *, private=True, maximum=2097152, reason=None):
        try:
            reader.read_secure(path, private=private, maximum=maximum)
        except reader.WindowsFileReadError as error:
            if reason and error.reason != reason:
                raise AssertionError('Unexpected rejection category') from error
            return {'reason': error.reason, 'winerror': getattr(error, 'winerror', None)}
        raise AssertionError('Unsafe file was accepted')

    def file(name):
        value = root / name
        value.write_bytes(b'synthetic-only')
        fixtures.trusted(value)
        return value

    try:
        def real_producer():
            receipt = validate_producer(args)
            environment = {key: value for key, value in os.environ.items() if key.upper() in {
                'SYSTEMROOT', 'WINDIR', 'SYSTEMDRIVE', 'COMSPEC', 'PATH', 'TEMP', 'TMP', 'USERPROFILE'}}
            completed = subprocess.run([str(args.producer.resolve()), str(root)], cwd=root, env=environment,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=False)
            observations['producerExecution'] = {'exitCode': completed.returncode, 'stdoutBytes': len(completed.stdout),
                'stderrBytes': len(completed.stderr), 'executableSha256': sha(args.producer), 'source': receipt}
            if completed.returncode != 0:
                raise AssertionError('Actual Client writer failed')
            target = root / 'codex-binding.json'
            observations['producerLeafSecurity'] = fixtures.security_observation(target)
            actual = reader.read_secure(target, private=True)
            parsed = json.loads(actual)
            if parsed != {'schemaVersion': 1, 'source': 'osir-client', 'available': False, 'errorCode': 'synthetic-contract'}:
                raise AssertionError('Actual producer output mismatch')
            # No ACL has been rewritten on this directory or producer output.
            reader.canonical_directory(root)
            return {'realClientWriteFunction': True, 'profileAclRewritten': False, 'producerAclRewritten': False}
        case('defaultProfileRealClientProducerAccepted', real_producer)
        def trusted():
            target = file('trusted.json')
            fixtures.trusted(target, '(A;;GR;;;WD)')
            assert reader.read_secure(target, private=True) == b'synthetic-only'
        case('trustedWritersAndForeignReadAccepted', trusted)
        def inherited():
            directory = root / 'inherited'
            directory.mkdir()
            fixtures.set_security(directory, 'P(A;OICI;FA;;;' + fixtures.sid_text(fixtures.user) + ')(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GR;;;WD)')
            target = directory / 'binding.json'
            target.write_bytes(b'synthetic-only')
            assert reader.read_secure(target, private=True) == b'synthetic-only'
        case('safeInheritedDaclAccepted', inherited)
        def foreign_writers():
            observed = []
            for index, mask in enumerate([2, 4, 0x10, 0x100, 0x10000, 0x40000, 0x80000, 0x10000000, 0x40000000]):
                target = file(f'foreign-{index}.json')
                fixtures.trusted(target, f'(A;;0x{mask:x};;;WD)')
                observed.append(rejects(target, reason='untrusted-writer'))
            return observed
        case('foreignMutationRightsRejected', foreign_writers)
        def unsafe_inherited():
            directory = root / 'unsafe-inherited'
            directory.mkdir()
            fixtures.set_security(directory, 'P(A;OICI;FA;;;' + fixtures.sid_text(fixtures.user) + ')(A;OICI;FA;;;WD)')
            target = directory / 'binding.json'
            target.write_bytes(b'synthetic-only')
            return rejects(target, reason='untrusted-writer')
        case('inheritedForeignWriterRejected', unsafe_inherited)
        def null_acl():
            target = file('null.json')
            fixtures.set_security(target, 'NO_ACCESS_CONTROL')
            return rejects(target, reason='null-dacl')
        case('nullDaclRejected', null_acl)
        def owner():
            target = file('owner.json')
            fixtures.set_security(target, 'P(A;;FA;;;' + fixtures.sid_text(fixtures.user) + ')', owner='BA')
            return rejects(target, reason='foreign-owner')
        case('foreignOwnerRejected', owner)
        case('directoryRejected', lambda: rejects(root))
        def hardlink():
            target = file('hardlink-original.json')
            os.link(target, root / 'hardlink-copy.json')
            return rejects(target, reason='links-or-size')
        case('multipleLinksRejected', hardlink)
        def symlink():
            target = file('symlink-original.json')
            link = root / 'symlink.json'
            if not fixtures.kernel.CreateSymbolicLinkW(str(link), str(target), 2):
                fixtures.api.checked(fixtures.kernel.CreateSymbolicLinkW(str(link), str(target), 0), 'fixture-create-symlink')
            return rejects(link, reason='reparse-or-file-type')
        case('leafReparseRejected', symlink)
        def junction():
            directory = root / 'junction-original'
            directory.mkdir()
            target = directory / 'binding.json'
            target.write_bytes(b'synthetic-only')
            fixtures.trusted(target)
            link = root / 'junction'
            fixtures.junction(link, directory)
            return rejects(link / target.name, reason='reparse-or-file-type')
        case('ancestorJunctionRejected', junction)
        case('oversizeRejected', lambda: rejects(file('size.json'), maximum=2, reason='links-or-size'))
        def nonprivate():
            target = file('config.toml')
            fixtures.trusted(target, '(A;;GW;;;WD)')
            assert reader.read_secure(target, private=False) == b'synthetic-only'
        case('nonPrivateConfigRetainsOwnerPolicy', nonprivate)
        def writer_conflict():
            target = file('writer-conflict.json')
            with ExitStack() as stack:
                handle = fixtures.kernel.CreateFileW(str(target), 0x40000000, 7, None, 3, 0, None)
                if handle in (None, C.c_void_p(-1).value):
                    fixtures.api.checked(False, 'fixture-writer-open')
                stack.callback(fixtures.api.close, handle)
                return rejects(target, reason='open-file')
        case('activeWriterRejected', writer_conflict)
        def held_handles():
            directory = root / 'pinned'
            directory.mkdir()
            target = directory / 'binding.json'
            target.write_bytes(b'synthetic-only')
            fixtures.trusted(target)
            other = file('replacement.json')
            with ExitStack() as stack:
                fixtures.api.open(str(directory), directory=True, stack=stack)
                fixtures.api.open(str(target), directory=False, stack=stack)
                denials = []
                for source, destination in [(other, target), (directory, root / 'renamed')]:
                    try:
                        os.replace(source, destination)
                    except OSError as error:
                        if error.winerror not in (5, 32, 33):
                            raise
                        denials.append(error.winerror)
                    else:
                        raise AssertionError('Pinned object was replaceable')
            assert reader.read_secure(target, private=True) == b'synthetic-only'
            assert not (root / 'renamed').exists()
            return {'replacementDeniedWinerrors': denials}
        case('heldLeafAndAncestorDenyReplacement', held_handles)
        def handle_leaks():
            target = file('leak.json')
            reader.read_secure(target, private=True)
            count = fixtures.handles()
            for _ in range(50):
                reader.read_secure(target, private=True)
            after = fixtures.handles()
            if after > count + 2:
                raise AssertionError('Handle growth')
            return {'before': count, 'after': after, 'iterations': 50}
        case('handlesClosed', handle_leaks)
    finally:
        after = fixtures.ancestry(profile)
        observations['profileAncestorsAfter'] = after
        report['checks']['profileAncestorSecurityUnchanged'] = before_ancestors == after
        # shutil.rmtree removes directory junctions without walking the target.
        # All altered ACLs were current-user writable synthetic fixtures.
        shutil.rmtree(root)
        report['checks']['syntheticSubtreeRemoved'] = not root.exists()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reader', type=Path, required=True)
    parser.add_argument('--reader-sha256', required=True)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--client-source-sha256', required=True)
    parser.add_argument('--producer', type=Path, required=True)
    parser.add_argument('--producer-sha256', required=True)
    parser.add_argument('--producer-receipt', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    report = {'schemaVersion': 1, 'scope': 'windows-descriptor-reader-contract', 'evidenceMode': 'native' if os.name == 'nt' else 'unsupported',
              'result': 'failed', 'sourceCommit': args.source_commit, 'readerSha256': sha(args.reader),
              'clientSourceSha256': args.client_source_sha256, 'nativeMachines': [], 'checks': {}, 'evidence': [],
              'limits': {'fullClientPublication': False, 'providerOrModelE2E': False, 'networkMayBeReachable': True,
                         'productionCredentialsUsed': False, 'ancestorAclRepair': False}}
    observations = {'python': platform.python_version(), 'platform': platform.platform()}
    try:
        if not re.fullmatch('[0-9a-f]{40}', args.source_commit) or not re.fullmatch('[0-9a-f]{64}', args.client_source_sha256):
            raise ValueError('Exact source bindings required')
        if report['readerSha256'] != args.reader_sha256:
            raise ValueError('Reader identity mismatch')
        run_native(args, report, observations)
        if set(report['checks']) == set(REQUIRED_CHECKS) and all(report['checks'].values()):
            report['result'] = 'passed'
    except Exception as error:
        observations['fatal'] = {'reason': getattr(error, 'reason', type(error).__name__), 'winerror': getattr(error, 'winerror', None)}
    raw = args.output / 'native-observations.json'
    raw.write_text(json.dumps(observations, indent=2) + '\n', encoding='utf-8')
    report['evidence'].append({'path': raw.name, 'size': raw.stat().st_size, 'sha256': sha(raw)})
    (args.output / 'report.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'result': report['result'], 'checks': report['checks'], 'output': str(args.output)}))
    return 0 if report['result'] == 'passed' else 1


if __name__ == '__main__':
    sys.exit(main())
