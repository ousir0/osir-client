"""Portable policy regressions. These tests are not Windows native evidence."""
from __future__ import annotations

import ctypes
import importlib.util
import os
import struct
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'connector/runtimes/codex/osir_binding_windows.py'
SPEC = importlib.util.spec_from_file_location('osir_windows_reader_policy_test', SOURCE)
reader = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reader)

USER = reader._well_known(5, 21, 1000)
OTHER = reader._well_known(1, 0)


def ace(mask, sid=OTHER, *, kind=0, flags=0):
    return struct.pack('<BBHI', kind, flags, 8 + len(sid), mask) + sid


def acl(*entries):
    body = b''.join(entries)
    return struct.pack('<BBHHH', 2, 0, 8 + len(body), len(entries), 0) + body


class PolicyTests(unittest.TestCase):
    def test_current_system_administrators_may_write_and_other_sids_may_read(self):
        for sid in [USER, reader._SYSTEM, reader._ADMINISTRATORS]:
            reader._validate_dacl(acl(ace(0x10000000, sid), ace(0x80020089)), USER)
        reader._validate_dacl(acl(ace(0x10000000, USER, flags=0x10)), USER)

    def test_every_foreign_writer_right_is_rejected_even_if_denied_elsewhere(self):
        for mask in [2, 4, 0x10, 0x100, 0x10000, 0x40000, 0x80000, 0x10000000, 0x40000000]:
            for flags in [0, 0x10]:
                with self.subTest(mask=hex(mask), inherited=bool(flags)):
                    with self.assertRaises(reader.WindowsFileReadError) as error:
                        reader._validate_dacl(acl(ace(mask, kind=1), ace(mask, flags=flags)), USER)
                    self.assertEqual(error.exception.reason, 'untrusted-writer')

    def test_inherit_only_does_not_grant_current_file_access(self):
        reader._validate_dacl(acl(ace(0x10000000, flags=8)), USER)

    def test_null_malformed_and_effective_unknown_aces_fail_closed(self):
        candidates = [None, b'', b'\x02' * 8, acl(ace(2, kind=5)), acl(ace(0, kind=9)), acl(ace(0)[:-1]), acl(ace(0, sid=b'bad-sid!'))]
        for raw in candidates:
            with self.subTest(raw=raw), self.assertRaises(OSError):
                reader._validate_dacl(raw, USER)

    def test_paths_allow_normal_local_producer_names_and_reject_aliasing(self):
        for path, expected in [(r'C:\Users\Example\.osir_client\anywhere\codex-binding.json', r'C:\Users\Example\.osir_client\anywhere\codex-binding.json'),
                               ('D:/测试/config.toml', 'D:\\测试\\config.toml'), (r'\\?\C:\Users\Example\config.toml', r'C:\Users\Example\config.toml')]:
            self.assertEqual(reader._normal_path(path), expected)
        for path in ['relative.json', 'C:relative.json', r'\\server\share\a', r'\\.\pipe\a', r'\\?\GLOBALROOT\Device\a',
                     r'C:\safe\..\a', r'C:\safe\file:stream', 'C:\\safe\\a.', 'C:\\safe\\a ', 'C:\\a\x00b']:
            with self.subTest(path=path), self.assertRaises(OSError):
                reader._normal_path(path)

    def test_errors_never_expose_paths_or_bytes_and_non_windows_does_not_fallback(self):
        self.assertEqual(str(reader.WindowsFileReadError('foreign-owner', 5)), 'OSIR local file is unsafe or unavailable.')
        if os.name != 'nt':
            with self.assertRaises(reader.WindowsFileReadError) as error:
                reader.read_secure(Path('/must-not-be-opened-secret-sentinel'))
            self.assertEqual(error.exception.reason, 'windows-required')
        for maximum in [-1, True, 2097153, '1024']:
            with self.subTest(maximum=maximum), self.assertRaises(OSError):
                reader.read_secure(Path('C:/fixture'), maximum=maximum)


class FakeAPI:
    """Synthetic handle boundary; never labelled native by tests or reports."""
    def __init__(self, *, changed=False, links=1, security_change=False):
        self.changed, self.links, self.security_change = changed, links, security_change
        self.closed, self.reads, self.security_calls, self.info_calls = [], 0, 0, 0
        self.kernel = SimpleNamespace(ReadFile=self.read_file)

    def user(self):
        return USER

    def open(self, path, *, directory, stack):
        handle = (path, directory)
        stack.callback(self.closed.append, handle)
        return handle

    def info(self, handle, path, *, directory):
        result = reader._FileInfo()
        result.attributes, result.volume, result.index_low, result.links = (0x10 if directory else 0), 1, 2, self.links
        if not directory:
            self.info_calls += 1
            result.size_low = 3
            result.written.low = int(self.changed and self.info_calls > 1)
        return result

    def security(self, handle, user, private):
        self.security_calls += 1
        return USER, b'changed' if self.security_change and self.security_calls > 1 else b'safe'

    def checked(self, result, _reason):
        if not result:
            raise AssertionError('unexpected fake failure')

    def read_file(self, handle, buffer, count, read, _overlapped):
        if handle[1]:
            raise AssertionError('read did not use the verified leaf handle')
        data = b'abc' if not self.reads else b''
        self.reads += 1
        ctypes.memmove(buffer, data, len(data))
        read._obj.value = len(data)
        return True


class HandleBoundaryTests(unittest.TestCase):
    def test_read_uses_verified_handle_and_closes_leaf_and_parents(self):
        api = FakeAPI()
        with patch.object(reader, '_api', return_value=api):
            self.assertEqual(reader.read_secure(Path('C:/fixture/binding.json'), private=True), b'abc')
        self.assertEqual(len(api.closed), 3)
        self.assertFalse(api.closed[0][1])
        self.assertEqual(api.security_calls, 2)

    def test_replacement_security_change_link_count_and_limit_are_not_success(self):
        for options, maximum in [({'changed': True}, 10), ({'security_change': True}, 10), ({'links': 2}, 10), ({}, 2)]:
            api = FakeAPI(**options)
            with self.subTest(options=options, maximum=maximum), patch.object(reader, '_api', return_value=api), self.assertRaises(OSError):
                reader.read_secure(Path('C:/fixture/binding.json'), private=True, maximum=maximum)
            self.assertEqual(len(api.closed), 3)


if __name__ == '__main__':
    unittest.main()
