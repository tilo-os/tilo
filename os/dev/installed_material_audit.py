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
        self.squash_images = 0

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
            raise AuditError('unexpected nested filesystem outside initramfs wrapper')
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

    def squash(self, image, scratch):
        """List then stream every regular payload; never extract image paths.

        Numeric listing is strict: ambiguous/unfamiliar paths or types fail.
        Every cat target and its ancestors are verified before unsquashfs runs.
        """
        records = {}
        with archive.child([archive.TOOLS['unsquashfs'], '-lln', '-dest', 'ROOT', str(image)],
                           limit=32 * 1024**2, scratch=scratch, deadline=self.deadline) as output:
            listing = bytearray()
            while data := output.read(archive.CHUNK):
                listing.extend(data)
        try:
            lines = listing.decode('utf-8', errors='strict').splitlines()
        except UnicodeError:
            raise AuditError('unsupported squashfs listing encoding') from None
        for line in lines:
            match = re.fullmatch(r'([dlpcbsp-])[rwxStTs-]{9} +[0-9]+/[0-9]+ +([0-9]+(?:, *[0-9]+)?) '
                                 r'[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2} (ROOT(?:/.*)?)', line)
            if not match:
                raise AuditError('unsupported squashfs listing')
            kind, size, path = match.groups()
            target = None
            if kind == 'l':
                if path.count(' -> ') != 1:
                    raise AuditError('ambiguous squashfs symlink')
                path, target = path.split(' -> ')
            name = path.removeprefix('ROOT').removeprefix('/')
            if (path in records or len(path.encode()) > 4096
                    or any(ord(c) < 32 or ord(c) == 127 for c in path)
                    or ' -> ' in name or (name and any(p in ('', '.', '..') for p in name.split('/')))):
                raise AuditError('unsafe or duplicate squashfs path')
            if len(records) >= MAX_ENTRIES:
                raise AuditError('squashfs entry limit exceeded')
            records[path] = (kind, size, name, target)
        if records.get('ROOT', (None,))[0] != 'd':
            raise AuditError('missing squashfs root directory')
        for path, (kind, size, name, target) in records.items():
            for parent in PurePosixPath(path).parents:
                if str(parent) != '.' and records.get(str(parent), (None,))[0] != 'd':
                    raise AuditError('squashfs ancestor is missing or redirected')
            if kind == '-':
                count = int(size)
                if count > MAX_TOTAL - self.bytes:
                    raise AuditError('squashfs expanded byte limit exceeded')
                with archive.child([archive.TOOLS['unsquashfs'], '-processors', '1', '-data-queue', '16',
                        '-frag-queue', '16', '-no-wildcards', '-cat', str(image), name],
                        limit=count, scratch=scratch, deadline=self.deadline) as output:
                    self.entry(name, stat.S_IFREG, count, output)
                    if output.read(1):
                        raise AuditError('squashfs payload exceeds listed size')
            elif kind == 'l':
                data = target.encode()
                self.entry(name, stat.S_IFLNK, len(data), io.BytesIO(data))
            else:
                self.entry(name or '.', stat.S_IFDIR if kind == 'd' else stat.S_IFIFO, 0, io.BytesIO())
        self.squash_images += 1

    def initramfs_entry(self, name, mode, size, stream, scratch):
        if name not in ('squash-root.img', 'squashfs-root.img'):
            return self.entry(name, mode, size, stream)
        if self.squash_images or not stat.S_ISREG(mode) or not 0 < size <= archive.MAX_IMAGE:
            raise AuditError('ambiguous or oversized squashfs wrapper')
        image = scratch / 'payload.squashfs'
        with image.open('xb') as output:
            remaining = size
            while remaining:
                data = archive.exact(stream, min(remaining, archive.CHUNK))
                output.write(data)
                remaining -= len(data)
        self.squash(image, scratch)

    def initramfs(self, data):
        if not 0 < len(data) <= archive.MAX_IMAGE:
            raise AuditError('initramfs size exceeds bounds')
        state = {'entries': 0, 'path_bytes': 0, 'archives': 0,
                 'deadline': self.deadline}
        with tempfile.TemporaryDirectory(prefix='tilo-installed-audit-') as tmp:
            scratch = Path(tmp)
            state['visitor'] = lambda name, mode, size, stream: self.initramfs_entry(name, mode, size, stream, scratch)
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
                'entries': state['entries'], 'squashfs_images': self.squash_images}


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

