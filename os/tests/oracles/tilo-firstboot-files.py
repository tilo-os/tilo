#!/usr/bin/python3
"""Compatibility oracle for firstboot file and home-seed tests.

The installed account transaction calls the compiled tilo-firstboot-files helper,
not this module. Keep the Python I/O oracle for interruption injection and
cross-checking the native seed receipt format.
Only complete files are published. Existing installer seeds are never replaced.
Destination directory descriptors anchor every edge, including ownership repair.
"""
import ctypes
import io
import os
import re
import secrets
import stat
import sys

DIR = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW


def publish(directory, name, writer):
    temporary = '.tilo-firstboot-' + secrets.token_hex(16)
    try:
        writer(temporary)
        # RENAME_NOREPLACE avoids replacement and the crash window where a
        # published intent still has a second link to its temporary.
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.renameat2(directory, os.fsencode(temporary), directory,
                          os.fsencode(name), 1) != 0:
            code = ctypes.get_errno()
            raise OSError(code, os.strerror(code))
        os.fsync(directory)
    finally:
        try:
            os.unlink(temporary, dir_fd=directory)
        except FileNotFoundError:
            pass
        os.fsync(directory)


def write_file(directory, name, source, mode, uid, gid):
    fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                 0o600, dir_fd=directory)
    try:
        with os.fdopen(fd, 'wb', closefd=False) as output:
            while block := source.read(1024 * 1024):
                output.write(block)
            output.flush()
        os.fchown(fd, uid, gid)
        os.fchmod(fd, mode)
        os.fsync(fd)
    finally:
        os.close(fd)


def defaults(source, destination, uid, gid):
    for name in os.listdir(source):
        metadata = os.stat(name, dir_fd=source, follow_symlinks=False)
        try:
            existing = os.stat(name, dir_fd=destination, follow_symlinks=False)
        except FileNotFoundError:
            existing = None
        if stat.S_ISDIR(metadata.st_mode):
            if existing is None:
                os.mkdir(name, stat.S_IMODE(metadata.st_mode), dir_fd=destination)
            child = os.open(name, DIR, dir_fd=destination)
            origin = os.open(name, DIR, dir_fd=source)
            try:
                defaults(origin, child, uid, gid)
                if existing is None:
                    os.fchmod(child, stat.S_IMODE(metadata.st_mode))
                os.fsync(child)
            finally:
                os.close(origin)
                os.close(child)
        elif existing is None:
            if stat.S_ISREG(metadata.st_mode):
                fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=source)
                with os.fdopen(fd, 'rb') as input_file:
                    publish(destination, name, lambda temporary: write_file(
                        destination, temporary, input_file,
                        stat.S_IMODE(metadata.st_mode), uid, gid))
            elif stat.S_ISLNK(metadata.st_mode):
                target = os.readlink(name, dir_fd=source)
                def link(temporary):
                    os.symlink(target, temporary, dir_fd=destination)
                    os.chown(temporary, uid, gid, dir_fd=destination,
                             follow_symlinks=False)
                publish(destination, name, link)
            else:
                raise ValueError('unsupported skeleton inode')
    os.fsync(destination)


def ownership(directory, uid, gid):
    for name in os.listdir(directory):
        metadata = os.stat(name, dir_fd=directory, follow_symlinks=False)
        if stat.S_ISDIR(metadata.st_mode):
            child = os.open(name, DIR, dir_fd=directory)
            try:
                ownership(child, uid, gid)
            finally:
                os.close(child)
        elif stat.S_ISREG(metadata.st_mode):
            if metadata.st_nlink != 1:
                raise ValueError('hardlinked home file')
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
            try:
                opened = os.fstat(fd)
                if opened.st_nlink != 1:
                    raise ValueError('hardlinked home file')
                # Ownership transfer must not restore set-ID bits stripped by chown.
                if (opened.st_uid, opened.st_gid) != (uid, gid):
                    os.fchown(fd, uid, gid)
                os.fsync(fd)
            finally:
                os.close(fd)
        elif stat.S_ISLNK(metadata.st_mode):
            if (metadata.st_uid, metadata.st_gid) != (uid, gid):
                os.chown(name, uid, gid, dir_fd=directory, follow_symlinks=False)
        else:
            raise ValueError('unsupported home inode')
    metadata = os.fstat(directory)
    if (metadata.st_uid, metadata.st_gid) != (uid, gid):
        os.fchown(directory, uid, gid)
    os.fsync(directory)


def main():
    os.umask(0)
    command, path = sys.argv[1:3]
    # /home is Fedora's trusted alias for /var/home; never resolve the final
    # account directory or any edge within it.
    parent = os.open(os.path.realpath(os.path.dirname(path)), DIR)
    try:
        name = os.path.basename(path)
        if command == 'intent':
            record = sys.stdin.buffer.read(1025)
            if not re.fullmatch(rb'[a-z_][a-z0-9_-]{0,31}:!tilo-firstboot-[a-f0-9]{32}\n', record):
                raise ValueError('incomplete intent record')
            publish(parent, name, lambda temporary: write_file(
                parent, temporary, io.BytesIO(record), 0o600, os.getuid(), os.getgid()))
        elif command == 'home':
            uid, gid = map(int, sys.argv[3:5])
            try:
                os.mkdir(name, 0o700, dir_fd=parent)
            except FileExistsError:
                pass
            home = os.open(name, DIR, dir_fd=parent)
            try:
                # Omit the skeleton when a separate helper must attest defaults.
                if len(sys.argv) == 6:
                    skel = os.open(sys.argv[5], DIR)
                    try:
                        defaults(skel, home, uid, gid)
                    finally:
                        os.close(skel)
                ownership(home, uid, gid)
            finally:
                os.close(home)
            os.fsync(parent)
        else:
            raise ValueError('unknown operation')
    finally:
        os.close(parent)


if __name__ == '__main__':
    main()
