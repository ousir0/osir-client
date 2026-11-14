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


class OwnerQualificationTests(unittest.TestCase):
    def identity(self, owner=USER, groups=(), restricted=()):
        return reader._TokenIdentity(USER, owner, tuple(groups), tuple(restricted), 'thread')

    def test_current_user_is_accepted_even_when_group_default_owner_is_ineligible(self):
        for identity in [self.identity(), self.identity(reader._ADMINISTRATORS),
                         self.identity(reader._ADMINISTRATORS, [(reader._ADMINISTRATORS, 0x10)], [(OTHER, 4)])]:
            self.assertIn(USER, reader._qualified_owners(identity))

    def test_only_trusted_enabled_owner_capable_default_group_is_qualified(self):
        for sid in [reader._ADMINISTRATORS, reader._SYSTEM]:
            identity = self.identity(sid, [(sid, 0x0F)])
            self.assertEqual(reader._qualified_owners(identity), frozenset({USER, sid}))
        # A user's ordinary group never becomes trusted solely because it can
        # legally own files or is designated the token's default owner.
        self.assertEqual(reader._qualified_owners(self.identity(OTHER, [(OTHER, 0x0F)])), frozenset({USER}))

    def test_membership_alone_or_deny_only_disabled_restricted_or_nondefault_is_insufficient(self):
        admin = reader._ADMINISTRATORS
        cases = [self.identity(admin), self.identity(USER, [(admin, 0x0F)]),
                 self.identity(admin, [(admin, 0x0F)], [(OTHER, 4)]),
                 self.identity(admin, [(admin, 0x0F), (admin, 0x0F)])]
        for attributes in [0, 2, 4, 8, 0x0A, 0x18, 0x1F, 0x2F]:
            cases.append(self.identity(admin, [(admin, attributes)]))
        for identity in cases:
            with self.subTest(identity=identity):
                self.assertEqual(reader._qualified_owners(identity), frozenset({USER}))

    def test_owner_qualification_does_not_expand_existing_dacl_writer_allowlist(self):
        identity = self.identity(reader._ADMINISTRATORS, [(reader._ADMINISTRATORS, 0x0F)])
        self.assertIn(reader._ADMINISTRATORS, reader._qualified_owners(identity))
        for mask in [2, 4, 0x10, 0x100, 0x10000, 0x40000, 0x80000, 0x10000000, 0x40000000]:
            with self.subTest(mask=mask), self.assertRaises(reader.WindowsFileReadError) as error:
                reader._validate_dacl(acl(ace(mask, OTHER)), identity.user)
            self.assertEqual(error.exception.reason, 'untrusted-writer')


class TokenBufferTests(unittest.TestCase):
    """Synthetic ABI/bounds checks, never Windows native evidence."""
    def make_api_and_buffer(self):
        offset, stride = reader._TokenGroups.groups.offset, ctypes.sizeof(reader._SidAndAttributes)
        buffer = ctypes.create_string_buffer(offset + 2 * stride + len(USER))
        reader._DWORD.from_buffer(buffer).value = 1
        pointer = ctypes.addressof(buffer) + offset + 2 * stride
        ctypes.memmove(pointer, USER, len(USER))
        group = reader._SidAndAttributes.from_buffer(buffer, offset)
        group.sid, group.attributes = pointer, 0x0F
        api = reader._Win32.__new__(reader._Win32)
        api._token_data = lambda *_: (buffer, len(buffer))
        api.sid = lambda p: ctypes.string_at(p, len(USER))
        return api, buffer, group

    def test_group_array_alignment_and_exact_sid_attributes_are_preserved(self):
        self.assertEqual(reader._TokenGroups.groups.offset, 8 if ctypes.sizeof(ctypes.c_void_p) == 8 else 4)
        api, buffer, _group = self.make_api_and_buffer()
        self.assertEqual(api._token_groups(None, 2), ((USER, 0x0F),))
        reader._DWORD.from_buffer(buffer).value = 0
        self.assertEqual(api._token_groups(None, 11), ())

    def test_group_counts_sid_bounds_and_duplicate_membership_fail_closed(self):
        for mutation in ['count', 'pointer', 'sid-length', 'duplicate']:
            api, buffer, group = self.make_api_and_buffer()
            if mutation == 'count':
                reader._DWORD.from_buffer(buffer).value = 4097
            elif mutation == 'pointer':
                group.sid = ctypes.addressof(buffer) + len(buffer) - 4
            elif mutation == 'sid-length':
                ctypes.c_ubyte.from_address(group.sid + 1).value = 15
            else:
                reader._DWORD.from_buffer(buffer).value = 2
                other = reader._SidAndAttributes.from_buffer(buffer, reader._TokenGroups.groups.offset + ctypes.sizeof(reader._SidAndAttributes))
                other.sid, other.attributes = group.sid, group.attributes
            with self.subTest(mutation=mutation), self.assertRaises(reader.WindowsFileReadError):
                api._token_groups(None, 2)


class EffectiveTokenTests(unittest.TestCase):
    """Exercise token selection/cleanup, without claiming native execution."""
    def make_api(self, thread_error=None):
        api = reader._Win32.__new__(reader._Win32)
        closed, requests, fallback = [], [], []
        api.kernel = SimpleNamespace(GetCurrentThread=lambda: -2, GetCurrentProcess=lambda: -1)
        def open_thread(_thread, _access, _as_self, out):
            out._obj.value = 11 if thread_error is None else None
            return thread_error is None
        def open_process(_process, _access, out):
            fallback.append(True)
            out._obj.value = 22
            return True
        api.advapi = SimpleNamespace(OpenThreadToken=open_thread, OpenProcessToken=open_process)
        api.close = lambda token: closed.append(token.value)
        api.checked = lambda result, reason: result or reader._deny(reason)
        def data(token, kind, _minimum):
            requests.append((token.value, kind))
            sid = USER if kind == 1 else reader._ADMINISTRATORS
            prefix = ctypes.sizeof(reader._SidAndAttributes) if kind == 1 else ctypes.sizeof(ctypes.c_void_p)
            buffer = ctypes.create_string_buffer(prefix + len(sid))
            pointer = ctypes.addressof(buffer) + prefix
            ctypes.memmove(pointer, sid, len(sid))
            reader._HANDLE.from_buffer(buffer).value = pointer
            return buffer, len(buffer)
        api._token_data = data
        api.sid = lambda pointer: ctypes.string_at(pointer, 8 + ctypes.c_ubyte.from_address(pointer + 1).value * 4)
        def groups(token, kind):
            requests.append((token.value, kind))
            return ((reader._ADMINISTRATORS, 0x0F),) if kind == 2 else ()
        api._token_groups = groups
        return api, closed, requests, fallback

    def test_thread_user_owner_and_membership_come_from_one_handle_and_it_is_closed(self):
        api, closed, requests, fallback = self.make_api()
        identity = api.identity()
        self.assertEqual(identity.source, 'thread')
        self.assertEqual(identity.user, USER)
        self.assertEqual(identity.default_owner, reader._ADMINISTRATORS)
        self.assertEqual({token for token, _ in requests}, {11})
        self.assertEqual(closed, [11])
        self.assertEqual(fallback, [])

    def test_only_no_thread_token_allows_process_fallback(self):
        for code in [1008, 5, 1346]:
            api, closed, requests, fallback = self.make_api(code)
            with self.subTest(code=code), patch.object(ctypes, 'get_last_error', return_value=code, create=True):
                if code == 1008:
                    self.assertEqual(api.identity().source, 'process')
                    self.assertEqual({token for token, _ in requests}, {22})
                    self.assertEqual(closed, [22])
                    self.assertEqual(fallback, [True])
                else:
                    with self.assertRaises(reader.WindowsFileReadError) as error:
                        api.identity()
                    self.assertEqual(error.exception.reason, 'thread-token')
                    self.assertEqual(fallback, [])
                    self.assertEqual(closed, [])


class FakeAPI:
    """Synthetic handle boundary; never labelled native by tests or reports."""
    def __init__(self, *, changed=False, links=1, security_change=False, token_change=False):
        self.changed, self.links, self.security_change = changed, links, security_change
        self.closed, self.reads, self.security_calls, self.info_calls = [], 0, 0, 0
        self.token_change, self.identity_calls = token_change, 0
        self.kernel = SimpleNamespace(ReadFile=self.read_file)

    def identity(self):
        self.identity_calls += 1
        owner = reader._ADMINISTRATORS if self.token_change and self.identity_calls > 1 else USER
        return reader._TokenIdentity(USER, owner, (), (), 'process')

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

    def security(self, handle, identity, private):
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
        for options, maximum in [({'changed': True}, 10), ({'security_change': True}, 10), ({'token_change': True}, 10), ({'links': 2}, 10), ({}, 2)]:
            api = FakeAPI(**options)
            with self.subTest(options=options, maximum=maximum), patch.object(reader, '_api', return_value=api), self.assertRaises(OSError):
                reader.read_secure(Path('C:/fixture/binding.json'), private=True, maximum=maximum)
            self.assertEqual(len(api.closed), 3)


if __name__ == '__main__':
    unittest.main()
