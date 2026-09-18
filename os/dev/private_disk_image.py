"""Exclusive sparse development disk files; never opens an existing disk.

Requires root on Linux and trusted, root-owned directory ancestry. This guards
against accidental reuse and unprivileged replacement, not a malicious root
process: a privileged installer opening a pathname must still be trusted.
Call validate() immediately before passing path to that installer. retain()
requests retention on successful context exit, which validates once more.
"""
import os
from pathlib import Path
import stat
import sys


def _identity(info):
    return info.st_dev, info.st_ino


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
