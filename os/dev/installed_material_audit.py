"""Bounded installed-state audit, not a claim of universal secret absence.

Caller must provide authenticated, read-only mounted roots and selected UKI.
Never follows filesystem symlinks, executes image code, or reports secret bytes.
"""
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import tempfile
import time

import development_boot as boot

archive = boot._module('installed_initramfs', 'initramfs-trust.py')
layers = boot._module('installed_layer_audit', 'audit-rootfs-layers.py')
AuditError = layers.AuditError
MAX_TOTAL = 8 * 1024**3
MAX_ENTRIES = 200000


class Scanner:
    def __init__(self, private_values=()):
        if any(not isinstance(v, bytes) or not 8 <= len(v) <= 65536 for v in private_values):
            raise AuditError('invalid bounded private comparison value')
        self.values = tuple(private_values)
        self.entries = self.bytes = 0
        self.deadline = time.monotonic() + 300

    def entry(self, name, mode, size, stream):
        self.entries += 1
        self.bytes += size
        if self.entries > MAX_ENTRIES or self.bytes > MAX_TOTAL or time.monotonic() > self.deadline:
            raise AuditError('installed material audit exceeds bounds')
        path = PurePosixPath(name)
        leaf = path.name
        forbidden = (leaf in ('registry-auth.json', 'auth.json', 'cosign.key', 'owner.key',
                              'efi-signing.key', 'tpm2-pcr-signing.key')
            or re.fullmatch(r'ssh_host_.*_key', leaf)
            or any(part in ('tilo-development', 'tilo-cache-secrets') for part in path.parts)
            or (leaf in ('machine-id', 'random-seed') and size > 0))
        if forbidden:
            raise AuditError('installed private-material path policy violation')
        if leaf in ('squash-root.img', 'squashfs-root.img', 'erofs-root.img'):
            raise AuditError('nested initramfs filesystem requires full audit support')
        if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode) or stat.S_ISLNK(mode)):
            # Device nodes in initramfs have no payload; no device is opened.
            if size:
                raise AuditError('unexpected special-file payload')
        reader = layers.Reader(stream, size, self.values, hashing=True)
        metadata = bytearray() if leaf == 'config.json' else None
        while data := reader.read(layers.CHUNK):
            if time.monotonic() > self.deadline:
                raise AuditError('installed material audit deadline exceeded')
            if metadata is not None:
                if len(metadata) + len(data) > layers.MAX_METADATA:
                    raise AuditError('credential JSON exceeds bound')
                metadata.extend(data)
        if reader.remaining:
            raise AuditError('truncated installed material')
        if reader.findings:
            raise AuditError('installed private material detected')
        if metadata is not None and stat.S_ISREG(mode):
            try:
                value = json.loads(metadata, object_pairs_hook=layers.unique)
            except (ValueError, UnicodeError):
                raise AuditError('unparseable installed config JSON') from None
            if isinstance(value, dict) and any(value.get(k) for k in ('auths', 'credsStore', 'credHelpers')):
                raise AuditError('installed registry credential configuration detected')

    def tree(self, root, prefix):
        root = Path(root)
        if root.is_symlink() or not root.is_dir():
            raise AuditError('audit root must be an actual directory')
        for directory, dirs, files in os.walk(root, followlinks=False):
            for leaf in sorted(dirs + files):
                path = Path(directory) / leaf
                info = path.lstat()
                name = prefix + '/' + path.relative_to(root).as_posix()
                if stat.S_ISLNK(info.st_mode):
                    value = os.fsencode(os.readlink(path))
                    self.entry(name, info.st_mode, len(value), io.BytesIO(value))
                elif stat.S_ISREG(info.st_mode):
                    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                    with os.fdopen(fd, 'rb') as stream:
                        actual = os.fstat(stream.fileno())
                        if (actual.st_dev, actual.st_ino, actual.st_mode, actual.st_size) != (info.st_dev, info.st_ino, info.st_mode, info.st_size):
                            raise AuditError('installed file changed during audit')
                        self.entry(name, info.st_mode, info.st_size, stream)
                        after = os.fstat(stream.fileno())
                        if (actual.st_mtime_ns, actual.st_ctime_ns, actual.st_size) != (after.st_mtime_ns, after.st_ctime_ns, after.st_size):
                            raise AuditError('installed file changed during audit')
                elif stat.S_ISDIR(info.st_mode):
                    self.entry(name, info.st_mode, 0, io.BytesIO())
                else:
                    raise AuditError('special file in installed mutable state')

    def initramfs(self, data):
        if not 0 < len(data) <= archive.MAX_IMAGE:
            raise AuditError('initramfs size exceeds bounds')
        state = {'entries': 0, 'path_bytes': 0, 'archives': 0,
                 'deadline': self.deadline, 'visitor': self.entry}
        with tempfile.TemporaryDirectory(prefix='tilo-installed-audit-') as tmp:
            scratch = Path(tmp)
            # A regular temporary input supports streaming trusted decompression.
            with tempfile.TemporaryFile(buffering=0) as source:
                source.write(data)
                source.seek(0)
                while magic := archive.next_magic(source):
                    if magic == b'070701':
                        archive.newc(source, magic, state, scratch)
                        continue
                    compression = next((name for signature, name in
                        ((b'\x1f\x8b', 'gzip'), (b'\xfd7zXZ\x00', 'xz'), (b'\x28\xb5\x2f\xfd', 'zstd'))
                        if magic.startswith(signature)), None)
                    if compression is None:
                        raise AuditError('unsupported initramfs format')
                    source.seek(-6, os.SEEK_CUR)
                    with archive.child([archive.TOOLS[compression], '-dc'], stdin=source,
                                       scratch=scratch, deadline=self.deadline) as expanded:
                        archive.archives(expanded, state, scratch)
                    break
        if not state['archives']:
            raise AuditError('no complete initramfs archive')
        return {'sha256': hashlib.sha256(data).hexdigest(), 'archives': state['archives'],
                'entries': state['entries']}


def audit_installed_material(etc_root, var_root, esp_root, uki, *, architecture='aarch64', private_values=()):
    scanner = Scanner(private_values)
    try:
        for root, prefix in ((etc_root, 'etc'), (var_root, 'var'), (esp_root, 'esp')):
            scanner.tree(root, prefix)
        fd = os.open(uki, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, 'rb') as source:
            info = os.fstat(source.fileno())
            if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= boot.UKI_LIMIT:
                raise AuditError('bounded regular selected UKI required')
            data = source.read(boot.UKI_LIMIT + 1)
        sections = boot.uki_sections(data, architecture)
        initrd = scanner.initramfs(sections['.initrd'])
    except (OSError, archive.VerificationError, ValueError) as error:
        if isinstance(error, AuditError):
            raise
        raise AuditError('installed material audit could not complete') from None
    return {'schema': 1, 'accepted': True, 'policy': 'installed-private-material-v1',
            'roots': ['mutable-etc', 'shared-var', 'esp'], 'files_and_entries': scanner.entries,
            'bytes_scanned': scanner.bytes, 'selected_uki_sha256': hashlib.sha256(data).hexdigest(),
            'initramfs': initrd, 'exact_private_values_checked': len(scanner.values),
            'coverage': ['private PEM envelopes', 'specified private paths', 'registry auth config',
                         'nonempty machine identity and random seed', 'provided exact private bytes'],
            'limitations': 'Bounded policy only; not universal secret absence. Symlinks inspected without following. Caller supplies authenticated read-only mounts.'}

