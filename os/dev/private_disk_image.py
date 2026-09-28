"""Exclusive sparse development disk files; never opens an existing disk.

Requires root on Linux and trusted, root-owned directory ancestry. This guards
against accidental reuse and unprivileged replacement, not a malicious root
process: a privileged installer opening a pathname must still be trusted.
Call validate() immediately before passing path to that installer. retain()
requests retention on successful context exit, which validates once more.
"""
from contextlib import contextmanager
import os
from pathlib import Path
import re
import stat
import sys
import time


def _identity(info):
    return info.st_dev, info.st_ino


def _disk_group():
    try:
        import grp
        return grp.getgrnam('disk').gr_gid
    except (ImportError, KeyError):
        return None


def _validated_loop_node(node, expected_device, created=True):
    """A node this module created is root:root 0600. One udev made on a host
    that runs it is root's, in group root or disk, with no access for others."""
    info = node.lstat()
    mode = stat.S_IMODE(info.st_mode)
    if created:
        owner_ok = mode == 0o600 and info.st_gid == 0
    else:
        owner_ok = mode & 0o7117 == 0 and info.st_gid in (0, _disk_group())
    if (not stat.S_ISBLK(info.st_mode) or not owner_ok or info.st_uid != 0
            or info.st_rdev != expected_device or info.st_nlink != 1):
        raise ValueError('loop partition node differs from kernel identity')
    return info


def _remove_owned_loop_node(node, expected_device, identity=None):
    info = _validated_loop_node(node, expected_device)
    if identity is not None and (info.st_dev, info.st_ino) != identity:
        raise ValueError('loop partition node changed before cleanup')
    node.unlink()


def _directory(path):
    """Walk from / using held descriptors and reject redirected ancestry."""
    descriptor = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in path.parts[1:]:
            info = os.fstat(descriptor)
            if info.st_uid != 0 or info.st_mode & 0o022:
                raise ValueError('disk directory ancestry must be root-owned and non-writable')
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        info = os.fstat(descriptor)
        if info.st_uid != 0 or info.st_mode & 0o077 or not stat.S_ISDIR(info.st_mode):
            raise ValueError('disk output directory must be root-owned and private')
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


@contextmanager
def loop_partition_nodes(loop, partition_numbers=(1, 2), *, devices=Path('/dev'),
                         sys_class=Path('/sys/class/block'), timeout=5.0,
                         sleep=time.sleep):
    """Materialize only the exact partitions of an already-held loop device.

    Minimal container and Kata ``/dev`` filesystems do not run udev, even though
    the kernel exposes newly scanned partitions in sysfs.  Read their kernel
    identities directly, create no other device nodes, and remove the exact
    nodes again before the caller detaches the loop device.
    """
    match = re.fullmatch(r'/dev/(loop[0-9]+)', str(loop))
    numbers = tuple(partition_numbers)
    if (match is None or not numbers or len(set(numbers)) != len(numbers)
            or any(type(number) is not int or number <= 0 for number in numbers)
            or timeout <= 0):
        raise ValueError('invalid loop partition node request')
    base = match.group(1)
    base_sysfs = sys_class / base
    expected = {f'{base}p{number}': number for number in numbers}
    deadline = time.monotonic() + timeout
    while True:
        observed = {
            path.name for path in sys_class.glob(f'{base}p*')
            if path.joinpath('partition').is_file()
        }
        if observed == set(expected):
            break
        if time.monotonic() >= deadline:
            raise ValueError('loop partition sysfs inventory differs')
        sleep(0.05)

    held = []
    try:
        for name, number in sorted(expected.items(), key=lambda item: item[1]):
            entry = sys_class / name
            if entry.resolve(strict=True).parent != base_sysfs.resolve(strict=True):
                raise ValueError('loop partition has an unexpected sysfs parent')
            if entry.joinpath('partition').read_text().strip() != str(number):
                raise ValueError('loop partition number differs')
            major_minor = entry.joinpath('dev').read_text().strip()
            if not re.fullmatch(r'[0-9]+:[0-9]+', major_minor):
                raise ValueError('loop partition has invalid kernel identity')
            major, minor = map(int, major_minor.split(':', 1))
            uevent = {}
            for line in entry.joinpath('uevent').read_text().splitlines():
                key, separator, value = line.partition('=')
                if not separator or not key or key in uevent:
                    raise ValueError('loop partition uevent is malformed')
                uevent[key] = value
            if (uevent.get('DEVNAME') != name
                    or ('PARTN' in uevent and uevent['PARTN'] != str(number))):
                raise ValueError('loop partition uevent identity differs')
            node = devices / name
            expected_device = os.makedev(major, minor)
            owned = False
            try:
                try:
                    info = _validated_loop_node(node, expected_device, created=False)
                except FileNotFoundError:
                    os.mknod(node, stat.S_IFBLK | 0o600, expected_device)
                    owned = True
                    info = _validated_loop_node(node, expected_device)
            except BaseException:
                if owned:
                    _remove_owned_loop_node(node, expected_device)
                raise
            held.append((node, info.st_dev, info.st_ino, info.st_rdev, owned))
        yield [devices / name for name, _ in sorted(expected.items(), key=lambda item: item[1])]
    finally:
        cleanup_error = None
        for node, device, inode, raw_device, owned in reversed(held):
            try:
                info = _validated_loop_node(node, raw_device, created=owned)
                if (info.st_dev, info.st_ino) != (device, inode):
                    raise ValueError('loop partition node changed before cleanup')
                if owned:
                    _remove_owned_loop_node(node, raw_device, (device, inode))
            except BaseException as error:
                cleanup_error = cleanup_error or error
        if cleanup_error is not None:
            raise cleanup_error


class PrivateDiskImage:
    """Use ``with PrivateDiskImage(absolute_path, bytes) as image``.

    The file is removed on failure or unless retain() was called. Descriptors
    remain held for the whole context. Allocation is sparse and does not prove
    sufficient backing storage for a later installer. The caller must impose
    its own capacity limits and must not resize, rename, chmod or hardlink it.
    """

    def __init__(self, path, size):
        self.path = Path(path)
        if not self.path.is_absolute() or '..' in self.path.parts or self.path.name in ('', '.', '..'):
            raise ValueError('disk path must be absolute and canonical')
        if type(size) is not int or size <= 0 or size % 512 or size > (1 << 63) - 1:
            raise ValueError('disk size must be positive, sector-aligned and fit signed off_t')
        self.size = size
        self._directory_fd = None
        self._fd = None
        self._created = None
        self._keep = False
        self._used = False

    def __enter__(self):
        if self._used:
            raise ValueError('disk allocation context cannot be reused')
        self._used = True
        if sys.platform != 'linux' or os.getuid() != 0 or os.geteuid() != 0:
            raise ValueError('disk allocation requires already-root Linux')
        self._directory_fd = _directory(self.path.parent)
        try:
            self._fd = os.open(self.path.name,
                os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                0o600, dir_fd=self._directory_fd)
            self._created = os.fstat(self._fd)
            os.ftruncate(self._fd, self.size)
            self.validate()
            return self
        except BaseException:
            self._finish(remove=True)
            raise

    def validate(self):
        """Check both held descriptors and the currently reachable pathname."""
        if self._fd is None or self._directory_fd is None:
            raise ValueError('disk allocation is not open')
        current_parent = _directory(self.path.parent)
        try:
            if _identity(os.fstat(current_parent)) != _identity(os.fstat(self._directory_fd)):
                raise ValueError('disk output directory was replaced')
        finally:
            os.close(current_parent)
        held = os.fstat(self._fd)
        named = os.stat(self.path.name, dir_fd=self._directory_fd, follow_symlinks=False)
        for info in (held, named):
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0
                    or info.st_mode & 0o077 or info.st_nlink != 1
                    or info.st_size != self.size or _identity(info) != _identity(self._created)):
                raise ValueError('disk file identity, ownership, permissions, links or size changed')
        return self.path

    def retain(self):
        """Request retention after successful, revalidated context exit."""
        self.validate()
        self._keep = True

    def fileno(self):
        """Borrow the validated held descriptor; caller must not close it."""
        self.validate()
        return self._fd

    def file_identity(self):
        """Return identity from the held descriptor after pathname validation."""
        self.validate()
        info = os.fstat(self._fd)
        return {'device': info.st_dev, 'inode': info.st_ino, 'size': info.st_size}

    def _finish(self, *, remove):
        try:
            if remove and self._created is not None and self._directory_fd is not None:
                try:
                    current = os.stat(self.path.name, dir_fd=self._directory_fd, follow_symlinks=False)
                    if _identity(current) == _identity(self._created):
                        os.unlink(self.path.name, dir_fd=self._directory_fd)
                except FileNotFoundError:
                    pass
        finally:
            if self._fd is not None:
                os.close(self._fd)
                self._fd = None
            if self._directory_fd is not None:
                os.close(self._directory_fd)
                self._directory_fd = None

    def __exit__(self, exc_type, exc, traceback):
        keep = False
        try:
            if exc_type is None and self._keep:
                self.validate()
                os.fsync(self._fd)
                os.fsync(self._directory_fd)
                keep = True
        finally:
            self._finish(remove=not keep)
        return False
