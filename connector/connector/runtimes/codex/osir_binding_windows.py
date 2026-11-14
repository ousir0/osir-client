"""Handle-based Windows reads for the local, non-secret Client descriptor.

No subprocesses, ACL repairs, privilege changes, business imports or content
logging. Config/catalog reads retain the caller's owner-only permission policy;
the stricter writer allowlist applies only to private=True descriptors.
"""
from __future__ import annotations

import ctypes as C
import ntpath
import os
import re
import struct
from contextlib import ExitStack
from functools import lru_cache
from pathlib import Path
from typing import NamedTuple

_DWORD, _WORD, _BYTE, _BOOL, _HANDLE = C.c_uint32, C.c_uint16, C.c_ubyte, C.c_int32, C.c_void_p
_MAXIMUM = 2 * 1024 * 1024
_READ_CONTROL = 0x20000
_REPARSE, _DIRECTORY = 0x400, 0x10
_OPEN_REPARSE, _BACKUP_SEMANTICS = 0x200000, 0x2000000
_MUTATING = 0x2 | 0x4 | 0x10 | 0x100 | 0x10000 | 0x40000 | 0x80000 | 0x10000000 | 0x40000000


class WindowsFileReadError(OSError):
    """Fixed public text; reason and native code contain no paths or bytes."""

    def __init__(self, reason: str, winerror: int | None = None):
        super().__init__('OSIR local file is unsafe or unavailable.')
        self.reason = reason
        if winerror is not None:
            self.winerror = winerror


def _deny(reason, winerror=None):
    raise WindowsFileReadError(reason, winerror)


def _sid_length(raw: bytes) -> int:
    if len(raw) < 8 or raw[0] != 1 or raw[1] > 15:
        _deny('invalid-sid')
    length = 8 + 4 * raw[1]
    if len(raw) != length:
        _deny('invalid-sid')
    return length


def _well_known(authority, *subauthorities):
    return bytes([1, len(subauthorities)]) + authority.to_bytes(6, 'big') + struct.pack('<' + 'I' * len(subauthorities), *subauthorities)


_SYSTEM = _well_known(5, 18)
_ADMINISTRATORS = _well_known(5, 32, 544)


class _TokenIdentity(NamedTuple):
    user: bytes
    default_owner: bytes
    groups: tuple[tuple[bytes, int], ...]
    restricting_sids: tuple[tuple[bytes, int], ...]
    source: str


def _qualified_owners(identity: _TokenIdentity) -> frozenset[bytes]:
    """An alternate owner must be this effective token's trusted default.

    Windows permits an owner-capable, non-deny-only group as TokenOwner.
    This boundary additionally requires ENABLED membership and excludes
    restricted tokens; mere membership or a well-known SID is insufficient.
    """
    owners = {identity.user}
    if identity.default_owner == identity.user or identity.restricting_sids:
        return frozenset(owners)
    if identity.default_owner not in {_SYSTEM, _ADMINISTRATORS}:
        return frozenset(owners)
    matching = [attributes for sid, attributes in identity.groups if sid == identity.default_owner]
    if len(matching) == 1:
        attributes = matching[0]
        if attributes & 0x0C == 0x0C and not attributes & (0x10 | 0x20):
            owners.add(identity.default_owner)
    return frozenset(owners)


def _validate_dacl(raw: bytes | None, user: bytes) -> None:
    """Conservative ALLOW-ACE check; never cancel a broad grant with a DENY."""
    if raw is None:
        _deny('null-dacl')
    if len(raw) < 8:
        _deny('invalid-dacl')
    revision, _, size, count, _ = struct.unpack_from('<BBHHH', raw)
    if revision not in (2, 4) or size != len(raw):
        _deny('invalid-dacl')
    allowed = {user, _SYSTEM, _ADMINISTRATORS}
    offset = 8
    for _ in range(count):
        if offset + 4 > size:
            _deny('invalid-ace')
        kind, flags, length = struct.unpack_from('<BBH', raw, offset)
        if length < 4 or length % 4 or offset + length > size:
            _deny('invalid-ace')
        # Inherit-only ACEs do not grant rights on this object. Unknown ACE
        # formats that DO apply cannot establish a safe writer allowlist.
        if not flags & 0x08:
            if kind not in (0, 1) or length < 16:
                _deny('unsupported-ace')
            mask = struct.unpack_from('<I', raw, offset + 4)[0]
            sid = raw[offset + 8:offset + length]
            _sid_length(sid)
            if kind == 0 and mask & _MUTATING and sid not in allowed:
                _deny('untrusted-writer')
        offset += length
    if offset > size:
        _deny('invalid-dacl')


def _normal_path(path) -> str:
    value = os.fspath(path)
    if not isinstance(value, str) or not value or '\x00' in value or len(value) > 32760:
        _deny('invalid-path')
    value = value.replace('/', '\\')
    # Canonical local drive paths only. Never turn a device/ADS/UNC name into
    # a normal file read. An ordinary extended DOS path is safe to normalize.
    if value.startswith('\\\\?\\') and re.match(r'^[A-Za-z]:\\', value[4:]):
        value = value[4:]
    drive, rest = ntpath.splitdrive(value)
    if not re.fullmatch('[A-Za-z]:', drive) or not rest.startswith('\\'):
        _deny('invalid-path')
    parts = rest[1:].split('\\') if rest != '\\' else []
    if any(not p or p in ('.', '..') or p.rstrip(' .') != p or ':' in p or any(ord(c) < 32 for c in p) for p in parts):
        _deny('invalid-path')
    return drive + '\\' + '\\'.join(parts)


class _FileTime(C.Structure):
    _fields_ = [('low', _DWORD), ('high', _DWORD)]


class _FileInfo(C.Structure):
    _fields_ = [('attributes', _DWORD), ('created', _FileTime), ('accessed', _FileTime), ('written', _FileTime),
                ('volume', _DWORD), ('size_high', _DWORD), ('size_low', _DWORD), ('links', _DWORD), ('index_high', _DWORD), ('index_low', _DWORD)]


class _SidAndAttributes(C.Structure):
    _fields_ = [('sid', _HANDLE), ('attributes', _DWORD)]


class _TokenGroups(C.Structure):
    # The array begins at offset 8 on x64 and 4 on x86, not sizeof(DWORD).
    _fields_ = [('count', _DWORD), ('groups', _SidAndAttributes * 1)]


class _Win32:
    def __init__(self):
        if os.name != 'nt':
            _deny('windows-required')
        self.kernel = C.WinDLL('kernel32', use_last_error=True)
        self.advapi = C.WinDLL('advapi32', use_last_error=True)
        pointer = C.POINTER(_HANDLE)
        self._bind(self.kernel, 'CreateFileW', [C.c_wchar_p, _DWORD, _DWORD, _HANDLE, _DWORD, _DWORD, _HANDLE], _HANDLE)
        self._bind(self.kernel, 'CloseHandle', [_HANDLE], _BOOL)
        self._bind(self.kernel, 'GetFileType', [_HANDLE], _DWORD)
        self._bind(self.kernel, 'GetFileInformationByHandle', [_HANDLE, C.POINTER(_FileInfo)], _BOOL)
        self._bind(self.kernel, 'GetFinalPathNameByHandleW', [_HANDLE, C.c_wchar_p, _DWORD, _DWORD], _DWORD)
        self._bind(self.kernel, 'ReadFile', [_HANDLE, _HANDLE, _DWORD, C.POINTER(_DWORD), _HANDLE], _BOOL)
        self._bind(self.kernel, 'GetCurrentProcess', [], _HANDLE)
        self._bind(self.kernel, 'GetCurrentThread', [], _HANDLE)
        self._bind(self.kernel, 'LocalFree', [_HANDLE], _HANDLE)
        self._bind(self.advapi, 'OpenThreadToken', [_HANDLE, _DWORD, _BOOL, pointer], _BOOL)
        self._bind(self.advapi, 'OpenProcessToken', [_HANDLE, _DWORD, pointer], _BOOL)
        self._bind(self.advapi, 'GetTokenInformation', [_HANDLE, _DWORD, _HANDLE, _DWORD, C.POINTER(_DWORD)], _BOOL)
        self._bind(self.advapi, 'GetSecurityInfo', [_HANDLE, _DWORD, _DWORD, pointer, pointer, pointer, pointer, pointer], _DWORD)
        self._bind(self.advapi, 'IsValidSid', [_HANDLE], _BOOL)
        self._bind(self.advapi, 'GetLengthSid', [_HANDLE], _DWORD)
        self._bind(self.advapi, 'IsValidAcl', [_HANDLE], _BOOL)
        self._bind(self.advapi, 'GetSecurityDescriptorDacl', [_HANDLE, C.POINTER(_BOOL), pointer, C.POINTER(_BOOL)], _BOOL)

    @staticmethod
    def _bind(library, name, arguments, result):
        function = getattr(library, name)
        function.argtypes, function.restype = arguments, result

    def checked(self, result, reason):
        if not result:
            _deny(reason, C.get_last_error())
        return result

    def close(self, handle):
        self.checked(self.kernel.CloseHandle(handle), 'close-handle')

    def open(self, path, *, directory, stack):
        flags = _OPEN_REPARSE | (_BACKUP_SEMANTICS if directory else 0)
        access = 0x80 if directory else 0x80000000 | _READ_CONTROL  # attributes / GENERIC_READ
        # Directory handles pin against rename/delete; the leaf also refuses
        # active writers. Transient publisher sharing conflicts fail closed.
        share = 3 if directory else 1
        handle = self.kernel.CreateFileW('\\\\?\\' + path, access, share, None, 3, flags, None)
        if handle == C.c_void_p(-1).value or handle is None:
            _deny('open-file', C.get_last_error())
        stack.callback(self.close, handle)
        return handle

    def info(self, handle, path, *, directory):
        if self.kernel.GetFileType(handle) != 1:
            _deny('not-disk-file')
        info = _FileInfo()
        self.checked(self.kernel.GetFileInformationByHandle(handle, C.byref(info)), 'file-information')
        if info.attributes & _REPARSE or bool(info.attributes & _DIRECTORY) != directory:
            _deny('reparse-or-file-type')
        buffer = C.create_unicode_buffer(32768)
        length = self.kernel.GetFinalPathNameByHandleW(handle, buffer, len(buffer), 0)
        if not 0 < length < len(buffer):
            _deny('final-path', C.get_last_error())
        if ntpath.normcase(buffer.value.rstrip('\\')) != ntpath.normcase(('\\\\?\\' + path).rstrip('\\')):
            _deny('redirected-path')
        return info

    def sid(self, pointer):
        if not pointer or not self.advapi.IsValidSid(pointer):
            _deny('invalid-sid')
        size = self.advapi.GetLengthSid(pointer)
        if not 8 <= size <= 68:
            _deny('invalid-sid')
        result = C.string_at(pointer, size)
        _sid_length(result)
        return result

    def _token_data(self, token, kind, minimum):
        size = _DWORD()
        if self.advapi.GetTokenInformation(token, kind, None, 0, C.byref(size)) or C.get_last_error() != 122 or not minimum <= size.value <= 65536:
            _deny('token-size')
        buffer = C.create_string_buffer(size.value)
        self.checked(self.advapi.GetTokenInformation(token, kind, buffer, len(buffer), C.byref(size)), 'token-information')
        if not minimum <= size.value <= len(buffer):
            _deny('token-size')
        return buffer, size.value

    def _token_sid(self, buffer, size, pointer):
        start = C.addressof(buffer)
        if not pointer or not start <= pointer <= start + size - 8:
            _deny('token-sid-bounds')
        header = C.string_at(pointer, 8)
        length = 8 + 4 * header[1]
        if header[0] != 1 or header[1] > 15 or pointer + length > start + size:
            _deny('token-sid-bounds')
        return self.sid(pointer)

    def _token_groups(self, token, kind):
        buffer, size = self._token_data(token, kind, C.sizeof(_DWORD))
        count = _DWORD.from_buffer(buffer).value
        offset, stride = _TokenGroups.groups.offset, C.sizeof(_SidAndAttributes)
        if count > 4096 or (count and offset + count * stride > size):
            _deny('token-group-bounds')
        result = []
        for index in range(count):
            group = _SidAndAttributes.from_buffer(buffer, offset + index * stride)
            result.append((self._token_sid(buffer, size, group.sid), group.attributes))
        if len({sid for sid, _ in result}) != len(result):
            _deny('duplicate-token-group')
        return tuple(result)

    def identity(self):
        # Query every field from ONE effective token. Only ERROR_NO_TOKEN
        # permits process fallback; never combine primary-token groups with
        # an impersonated thread's user/default owner.
        token, source = _HANDLE(), 'thread'
        if not self.advapi.OpenThreadToken(self.kernel.GetCurrentThread(), 8, True, C.byref(token)):
            if C.get_last_error() != 1008:
                _deny('thread-token', C.get_last_error())
            self.checked(self.advapi.OpenProcessToken(self.kernel.GetCurrentProcess(), 8, C.byref(token)), 'process-token')
            source = 'process'
        try:
            user_buffer, user_size = self._token_data(token, 1, C.sizeof(_SidAndAttributes))
            owner_buffer, owner_size = self._token_data(token, 4, C.sizeof(_HANDLE))
            user = self._token_sid(user_buffer, user_size, _HANDLE.from_buffer(user_buffer).value)
            owner = self._token_sid(owner_buffer, owner_size, _HANDLE.from_buffer(owner_buffer).value)
            return _TokenIdentity(user, owner, self._token_groups(token, 2), self._token_groups(token, 11), source)
        finally:
            self.close(token)

    def user(self):
        return self.identity().user

    def security(self, handle, identity, private):
        owner, dacl, descriptor = _HANDLE(), _HANDLE(), _HANDLE()
        code = self.advapi.GetSecurityInfo(handle, 1, 1 | (4 if private else 0), C.byref(owner), None, C.byref(dacl) if private else None, None, C.byref(descriptor))
        if code:
            _deny('security-information', code)
        try:
            actual_owner = self.sid(owner)
            if actual_owner not in _qualified_owners(identity):
                _deny('foreign-owner')
            raw = None
            if private:
                present, defaulted = _BOOL(), _BOOL()
                self.checked(self.advapi.GetSecurityDescriptorDacl(descriptor, C.byref(present), C.byref(dacl), C.byref(defaulted)), 'descriptor-dacl')
                if not present.value or not dacl.value:
                    _deny('null-dacl')
                if not self.advapi.IsValidAcl(dacl):
                    _deny('invalid-dacl')
                size = struct.unpack('<H', C.string_at(dacl.value + 2, 2))[0]
                if size < 8:
                    _deny('invalid-dacl')
                raw = C.string_at(dacl, size)
                _validate_dacl(raw, identity.user)
            return actual_owner, raw
        finally:
            if descriptor.value and self.kernel.LocalFree(descriptor):
                _deny('free-security-descriptor')


@lru_cache(maxsize=1)
def _api():
    # Only ABI bindings are cached, never owner/ACL/path/content observations.
    return _Win32()


def _signature(info):
    return (info.attributes, info.volume, info.index_high, info.index_low, info.links,
            info.size_high, info.size_low, info.written.high, info.written.low)


def _directories(api, path, stack, *, include_leaf):
    drive, tail = ntpath.splitdrive(path)
    parts = tail[1:].split('\\') if tail != '\\' else []
    if not include_leaf:
        if not parts:
            _deny('file-required')
        parts.pop()
    current, held = drive + '\\', []
    for part in [None, *parts]:
        if part is not None:
            current = ntpath.join(current, part)
        handle = api.open(current, directory=True, stack=stack)
        info = api.info(handle, current, directory=True)
        held.append((handle, current, _signature(info)))
    return held


def _recheck_directories(api, held):
    for handle, path, signature in held:
        if _signature(api.info(handle, path, directory=True))[:5] != signature[:5]:
            _deny('directory-changed')


def canonical_directory(path: Path) -> Path:
    """Validate a local absolute directory without following a reparse point."""
    api, value = _api(), _normal_path(path)
    with ExitStack() as stack:
        held = _directories(api, value, stack, include_leaf=True)
        _recheck_directories(api, held)
    return Path(value)


def read_secure(path: Path, *, private=False, maximum=_MAXIMUM) -> bytes:
    """Read from the same owner-checked handle, or raise a fixed OSError."""
    if type(maximum) is not int or not 0 <= maximum <= _MAXIMUM or type(private) is not bool:
        _deny('invalid-read-limit')
    api, value = _api(), _normal_path(path)
    identity = api.identity()
    with ExitStack() as stack:
        held = _directories(api, value, stack, include_leaf=False)
        handle = api.open(value, directory=False, stack=stack)
        before = api.info(handle, value, directory=False)
        if before.links != 1 or (before.size_high << 32 | before.size_low) > maximum:
            _deny('links-or-size')
        security = api.security(handle, identity, private)
        data = bytearray()
        while len(data) <= maximum:
            length = min(65536, maximum + 1 - len(data))
            buffer, read = C.create_string_buffer(length), _DWORD()
            api.checked(api.kernel.ReadFile(handle, buffer, length, C.byref(read), None), 'read-file')
            if read.value > length:
                _deny('invalid-read-size')
            if not read.value:
                break
            data.extend(buffer.raw[:read.value])
        after = api.info(handle, value, directory=False)
        if len(data) > maximum or len(data) != (before.size_high << 32 | before.size_low) or _signature(before) != _signature(after):
            _deny('file-changed')
        if api.security(handle, identity, private) != security:
            _deny('security-changed')
        _recheck_directories(api, held)
        if api.identity() != identity:
            _deny('token-changed')
        return bytes(data)
