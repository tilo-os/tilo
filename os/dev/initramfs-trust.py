"""Read-only, bounded PCR-public-key verification for newc dracut initramfs.

Supports newc with gzip/xz/zstd main archive and optional squashfs payload.
No image program runs and no archive filesystem is extracted. This verifies
one exact consumed key path, not general secret absence or bootability.
"""
from contextlib import contextmanager
import hashlib
import os
import resource
import re
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import tempfile
import threading
import time

KEY = 'usr/share/tilo/tpm2-pcr-signing.public.pem'
MAX_IMAGE = 512 * 1024**2
MAX_EXPANDED = 4 * 1024**3
MAX_KEY = 65536
CHUNK = 1024 * 1024
TIMEOUT = 120
TOOLS = {'gzip': '/usr/bin/gzip', 'xz': '/usr/bin/xz', 'zstd': '/usr/bin/zstd',
         'unsquashfs': '/usr/bin/unsquashfs'}


class VerificationError(ValueError):
    pass


class Bounded:
    def __init__(self, stream, limit):
        self.stream, self.remaining = stream, limit

    def read(self, count):
        if count < 0 or count > CHUNK:
            raise VerificationError('unbounded read')
        data = self.stream.read(min(count, self.remaining + 1))
        self.remaining -= len(data)
        if self.remaining < 0:
            raise VerificationError('expanded data exceeds bound')
        return data


def exact(stream, count):
    result = bytearray()
    while len(result) < count:
        part = stream.read(min(CHUNK, count - len(result)))
        if not part:
            raise VerificationError('truncated archive')
        result.extend(part)
    return bytes(result)


def child_limits():
    resource.setrlimit(resource.RLIMIT_AS, (2 * 1024**3, 2 * 1024**3))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


@contextmanager
def child(arguments, *, stdin=None, limit=MAX_EXPANDED, scratch, deadline):
    process = subprocess.Popen(arguments, stdin=stdin, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, start_new_session=True, preexec_fn=child_limits,
        env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'TMPDIR': str(scratch)})
    expired = threading.Event()
    def stop():
        # An exited leader may leave a descendant holding stdout open. Kill the
        # owned session even then, so the deadline can release a blocked read.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    def expire():
        expired.set()
        stop()
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        stop()
        process.wait(timeout=10)
        process.stdout.close()
        raise VerificationError('verification deadline exceeded')
    timer = threading.Timer(remaining, expire)
    timer.start()
    try:
        yield Bounded(process.stdout, limit)
        if process.wait(timeout=10) or expired.is_set():
            raise VerificationError('trusted archive tool failed or timed out')
    finally:
        timer.cancel()
        stop()
        process.wait(timeout=10)
        process.stdout.close()


def next_magic(stream):
    # initramfs pads early CPIO and the final archive with zeros.
    for _ in range(65536):
        first = stream.read(1)
        if not first:
            return b''
        if first != b'\0':
            return first + exact(stream, 5)
    raise VerificationError('excessive archive padding')


def newc(stream, magic, state, scratch):
    seen = set()
    while True:
        if time.monotonic() >= state['deadline']:
            raise VerificationError('verification deadline exceeded')
        if magic != b'070701':
            raise VerificationError('only newc CPIO is supported')
        raw = exact(stream, 104)
        if any(byte not in b'0123456789abcdefABCDEF' for byte in raw):
            raise VerificationError('invalid newc numeric field')
        try:
            fields = [int(raw[i:i + 8], 16) for i in range(0, 104, 8)]
        except ValueError:
            raise VerificationError('invalid newc header') from None
        _, mode, _, _, links, _, size, _, _, _, _, namesize, checksum = fields
        if not 1 <= namesize <= 4096 or checksum or size > MAX_EXPANDED:
            raise VerificationError('invalid or oversized newc entry')
        raw_name = exact(stream, namesize)
        if raw_name[-1:] != b'\0' or b'\0' in raw_name[:-1]:
            raise VerificationError('invalid CPIO name')
        try:
            name = raw_name[:-1].decode('utf-8')
        except UnicodeError:
            raise VerificationError('invalid CPIO name') from None
        exact(stream, (-(110 + namesize)) % 4)
        if name == 'TRAILER!!!':
            if size:
                raise VerificationError('invalid CPIO trailer')
            state['archives'] += 1
            return
        while name.startswith('./'):
            name = name[2:]
        if (not name or name.startswith('/') or name in seen
                or (name != '.' and any(part in ('', '.', '..') for part in name.split('/')))):
            raise VerificationError('unsafe or duplicate CPIO path')
        seen.add(name)
        state['path_bytes'] += namesize
        if state['path_bytes'] > 32 * 1024**2:
            raise VerificationError('CPIO path metadata exceeds bound')
        state['entries'] += 1
        if state['entries'] > 100000:
            raise VerificationError('too many CPIO entries')
        if visitor := state.get('visitor'):
            visitor(name, mode, size, stream)
            exact(stream, (-size) % 4)
            magic = exact(stream, 6)
            continue
        if name in ('usr', 'usr/share', 'usr/share/tilo') and not stat.S_ISDIR(mode):
            state['key_parent_redirect'] = True
        squash = name in ('squash-root.img', 'squashfs-root.img')
        if name == 'erofs-root.img':
            raise VerificationError('EROFS initramfs is unsupported')
        selected = name == KEY or squash
        if selected and (not stat.S_ISREG(mode) or links != 1):
            raise VerificationError('selected artifact must be a single regular file')
        if name == KEY:
            if state['keys'] or state['squash'] is not None:
                raise VerificationError('duplicate or conflicting key storage')
            if not 0 < size <= MAX_KEY:
                raise VerificationError('PCR key exceeds bound or is empty')
            state['keys'].append(exact(stream, size))
        elif squash:
            if state['keys'] or state['squash'] is not None or not 0 < size <= MAX_IMAGE:
                raise VerificationError('ambiguous or oversized squashfs payload')
            path = scratch / 'squash.img'
            with path.open('xb') as output:
                remaining = size
                while remaining:
                    data = exact(stream, min(CHUNK, remaining))
                    output.write(data)
                    remaining -= len(data)
            state['squash'] = path
        else:
            remaining = size
            while remaining:
                data = exact(stream, min(CHUNK, remaining))
                remaining -= len(data)
        exact(stream, (-size) % 4)
        magic = exact(stream, 6)


def archives(stream, state, scratch):
    while magic := next_magic(stream):
        newc(stream, magic, state, scratch)


def squash_key(path, scratch, deadline, expected_size):
    # -cat resolves symlinks. First require the exact root/ancestor/key chain to
    # consist only of directories and a regular leaf, using the trusted tool's
    # fixed-path numeric listing. Reject unfamiliar output instead of guessing.
    expected = {'ROOT': 'd', 'ROOT/usr': 'd', 'ROOT/usr/share': 'd',
                'ROOT/usr/share/tilo': 'd', 'ROOT/' + KEY: '-'}
    with child([TOOLS['unsquashfs'], '-lln', '-no-wildcards', '-dest', 'ROOT',
                str(path), KEY], limit=MAX_KEY, scratch=scratch, deadline=deadline) as output:
        listing = bytearray()
        while part := output.read(CHUNK):
            listing.extend(part)
    seen = set()
    for line in listing.decode('ascii', errors='strict').splitlines():
        match = re.fullmatch(r'([d-])[rwxStTs-]{9} +[0-9]+/[0-9]+ +([0-9]+) '
                             r'[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2} (ROOT(?:/[a-zA-Z0-9./_-]+)?)', line)
        if not match or match[3] in seen or expected.get(match[3]) != match[1]:
            raise VerificationError('squashfs key path is redirected or listing is unsupported')
        if match[3] == 'ROOT/' + KEY and int(match[2]) != expected_size:
            raise VerificationError('squashfs public key size differs')
        seen.add(match[3])
    if seen != set(expected):
        raise VerificationError('squashfs regular key path is incomplete')
    with child([TOOLS['unsquashfs'], '-processors', '1', '-data-queue', '16',
                '-frag-queue', '16', '-cat', str(path), KEY],
               limit=MAX_KEY, scratch=scratch, deadline=deadline) as output:
        key = bytearray()
        while part := output.read(CHUNK):
            key.extend(part)
    return bytes(key)


def verify(image: Path, expected_key: bytes, *, scratch_parent=None):
    deadline = time.monotonic() + TIMEOUT
    if not isinstance(expected_key, bytes) or not 0 < len(expected_key) <= MAX_KEY:
        raise VerificationError('bounded independently validated public key required')
    parent = Path(scratch_parent or os.environ.get('TMPDIR', tempfile.gettempdir()))
    if not parent.is_absolute() or shutil.disk_usage(parent).free < MAX_IMAGE + 64 * 1024**2:
        raise VerificationError('bounded private scratch capacity unavailable')
    descriptor = os.open(image, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb', buffering=0) as source, tempfile.TemporaryDirectory(prefix='tilo-initramfs-', dir=parent) as tmp:
        scratch = Path(tmp)
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_IMAGE:
            raise VerificationError('bounded regular initramfs required')
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
        source.seek(0)
        state = {'entries': 0, 'path_bytes': 0, 'archives': 0, 'keys': [], 'squash': None, 'key_parent_redirect': False, 'deadline': deadline}
        compression = 'none'
        while magic := next_magic(source):
            if magic == b'070701':
                newc(source, magic, state, scratch)
                continue
            if magic.startswith(b'\x1f\x8b'):
                compression = 'gzip'
            elif magic.startswith(b'\xfd7zXZ\x00'):
                compression = 'xz'
            elif magic.startswith(b'\x28\xb5\x2f\xfd'):
                compression = 'zstd'
            else:
                raise VerificationError('unsupported initramfs compression or format')
            source.seek(-6, os.SEEK_CUR)
            with child([TOOLS[compression], '-dc'], stdin=source, scratch=scratch, deadline=deadline) as expanded:
                archives(expanded, state, scratch)
            break
        if not state['archives']:
            raise VerificationError('no complete CPIO archive')
        if state['keys'] and state['key_parent_redirect']:
            raise VerificationError('PCR key parent is redirected')
        if state['squash'] is not None:
            state['keys'].append(squash_key(state['squash'], scratch, deadline, len(expected_key)))
        if state['keys'] != [expected_key]:
            raise VerificationError('missing, duplicate, or mismatched PCR key')
        after = os.fstat(source.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise VerificationError('initramfs changed during verification')
        return {'schema': 1, 'initramfs_sha256': digest,
                'pcr_public_sha256': hashlib.sha256(expected_key).hexdigest(),
                'compression': compression, 'cpio_archives': state['archives'],
                'cpio_entries': state['entries'], 'squashfs': state['squash'] is not None,
                'verified_path': KEY, 'matching_occurrences': 1,
                'scope': 'exact PCR key at fixed initramfs path; no general absence or bootability proof'}
