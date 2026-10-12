#!/usr/bin/env python3
"""Atomically autoclearing disposable native store and dev/inode retirement observer.

Only the native fixture owns backing files. No external disk is formatted, no
path/name heuristic establishes retirement, and unsupported ioctl is a refusal.
"""
import argparse
import ctypes
import errno
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile

LOOP_CONFIGURE = 0x4C0A
LOOP_GET_STATUS64 = 0x4C05
LOOP_CTL_GET_FREE = 0x4C82
AUTOCLEAR = 4


class Info(ctypes.Structure):
    _fields_ = [('device', ctypes.c_uint64), ('inode', ctypes.c_uint64),
                ('rdevice', ctypes.c_uint64), ('offset', ctypes.c_uint64),
                ('sizelimit', ctypes.c_uint64), ('number', ctypes.c_uint32),
                ('encrypt_type', ctypes.c_uint32), ('encrypt_key_size', ctypes.c_uint32),
                ('flags', ctypes.c_uint32), ('file_name', ctypes.c_ubyte * 64),
                ('crypt_name', ctypes.c_ubyte * 64), ('key', ctypes.c_ubyte * 32),
                ('init', ctypes.c_uint64 * 2)]


class Config(ctypes.Structure):
    _fields_ = [('fd', ctypes.c_uint32), ('block_size', ctypes.c_uint32),
                ('info', Info), ('reserved', ctypes.c_uint64 * 8)]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, 'duplicate native association field')
        result[key] = value
    return result


def protected_directory(path):
    require(path.is_absolute() and path.resolve() == path, 'native store path is redirected')
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        metadata = os.fstat(fd)
        require(metadata.st_uid == 0 and not metadata.st_mode & 0o022,
                'native store directory is not root-controlled')
        return fd
    except BaseException:
        os.close(fd)
        raise


def size_bytes(value):
    require(type(value) is str and re.fullmatch('[0-9]+[MG]?', value), 'invalid native store size')
    multiplier = {'M': 1024 ** 2, 'G': 1024 ** 3}.get(value[-1], 1)
    number = int(value[:-1] if value[-1] in 'MG' else value) * multiplier
    require(256 * 1024 ** 2 <= number <= 16 * 1024 ** 3, 'native store size outside bounds')
    return number


def associations(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_uid == 0
                and before.st_nlink == 1 and stat.S_IMODE(before.st_mode) == 0o600
                and 0 < before.st_size <= 4096, 'native association journal is not protected')
        data = bytearray()
        while part := os.read(fd, 4097 - len(data)):
            data.extend(part)
            require(len(data) <= 4096, 'native association journal exceeds bound')
        after = os.fstat(fd)
        require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
                == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
                'native association journal changed while held')
    finally:
        os.close(fd)
    value = json.loads(data, object_pairs_hook=unique)
    require(type(value) is dict and set(value) == {'schema', 'entries'}
            and type(value['schema']) is int and value['schema'] == 1
            and type(value['entries']) is list and 1 <= len(value['entries']) <= 2,
            'native association journal shape differs')
    keys = set()
    for row in value['entries']:
        require(type(row) is dict and set(row) == {'device', 'inode', 'number'}
                and all(type(row[field]) is int and 0 <= row[field] < 2 ** 64
                        for field in ('device', 'inode'))
                and row['inode'] > 0 and type(row['number']) is int
                and 0 <= row['number'] <= 1048575, 'native association identity is invalid')
        key = (row['device'], row['inode'])
        require(key not in keys, 'duplicate native backing identity')
        keys.add(key)
    return value


def record(directory, entry):
    journal = directory / 'associations.json'
    value = associations(journal) if journal.exists() else {'schema': 1, 'entries': []}
    require(len(value['entries']) < 2, 'native association journal is already complete')
    value['entries'].append(entry)
    fd, name = tempfile.mkstemp(prefix='.associations-', dir=directory)
    try:
        os.fchmod(fd, 0o600)
        raw = json.dumps(value, sort_keys=True).encode()
        require(len(raw) <= 4096, 'native association journal exceeds bound')
        with os.fdopen(fd, 'wb') as stream:
            fd = None
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, journal)
        parent = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
    finally:
        if fd is not None:
            os.close(fd)
        if os.path.exists(name):
            os.unlink(name)


def status(fd):
    raw = bytearray(ctypes.sizeof(Info))
    try:
        fcntl.ioctl(fd, LOOP_GET_STATUS64, raw, True)
    except OSError as error:
        if error.errno == errno.ENXIO:
            return None
        raise
    return Info.from_buffer_copy(raw)


def loop_fd(number, temporary):
    # Never depend on a container's /dev node inventory. This private node opens
    # only the kernel's loop major with the number actually selected/observed.
    path = temporary / str(number)
    os.mknod(path, stat.S_IFBLK | 0o600, os.makedev(7, number))
    fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        metadata = os.fstat(fd)
        require(stat.S_ISBLK(metadata.st_mode) and metadata.st_rdev == os.makedev(7, number),
                'native loop handle differs')
        return fd
    except BaseException:
        os.close(fd)
        raise


def create(directory, name, size, journal_directory):
    require(os.geteuid() == 0 and name in ('a', 'b'), 'native autoclear setup requires its root owner')
    require(ctypes.sizeof(Info) == 232 and ctypes.sizeof(Config) == 304,
            'native loop ABI is unsupported')
    directory = directory.absolute()
    journal_parent = protected_directory(journal_directory)
    os.close(journal_parent)
    parent = protected_directory(directory)
    backing = control = device = None
    try:
        backing = os.open(name + '.img', os.O_RDWR | os.O_CREAT | os.O_EXCL
                          | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=parent)
        os.ftruncate(backing, size_bytes(size))
        metadata = os.fstat(backing)
        require(metadata.st_uid == 0 and metadata.st_nlink == 1, 'native backing ownership differs')
        control = os.open('/dev/loop-control', os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
        require(stat.S_ISCHR(os.fstat(control).st_mode)
                and os.fstat(control).st_rdev == os.makedev(10, 237), 'native loop controller differs')
        number = fcntl.ioctl(control, LOOP_CTL_GET_FREE)
        require(type(number) is int and 0 <= number <= 1048575, 'native loop number outside bounds')
        with tempfile.TemporaryDirectory(prefix='.loop-node-', dir=directory) as temporary:
            device = loop_fd(number, Path(temporary))
            # Persist the backing identity BEFORE any ioctl can attach it.
            # This is an ownership record, never a successful qualification.
            record(journal_directory, {'device': metadata.st_dev, 'inode': metadata.st_ino, 'number': number})
            config = Config()
            config.fd = backing
            config.info.flags = AUTOCLEAR
            fcntl.ioctl(device, LOOP_CONFIGURE, bytes(config))
            observed = status(device)
            require(observed is not None and observed.device == metadata.st_dev
                    and observed.inode == metadata.st_ino and observed.number == number
                    and observed.flags & AUTOCLEAR, 'native loop did not atomically autoclear')
            os.mkdir(name, 0o755, dir_fd=parent)
            # Hold the configured loop FD through BOTH formatter and mount.
            # Any setup death closes it; before mount there is no other owner.
            subprocess.run(['mkfs.btrfs', '-q', '-f', '/proc/self/fd/' + str(device)],
                           check=True, pass_fds=(device,), timeout=90)
            subprocess.run(['mount', '-t', 'btrfs', '/proc/self/fd/' + str(device),
                            '/proc/self/fd/' + str(parent) + '/' + name],
                           check=True, pass_fds=(device, parent), timeout=30)
            require(status(device).flags & AUTOCLEAR, 'native mounted loop lost autoclear')
    finally:
        for fd in (device, control, backing, parent):
            if fd is not None:
                os.close(fd)


def observe(journal):
    require(os.geteuid() == 0, 'native loop observer requires its root owner')
    expected = associations(journal)
    wanted = {(row['device'], row['inode']) for row in expected['entries']}
    with tempfile.TemporaryDirectory(prefix='tilo-loop-observe-') as temporary:
        queried = set()
        def check(number):
            require(number <= 1048575, 'native observed loop number outside bounds')
            if number in queried:
                return
            fd = loop_fd(number, Path(temporary))
            try:
                current = status(fd)
                require(current is None or (current.device, current.inode) not in wanted,
                        'owned native backing remains associated')
                queried.add(number)
            finally:
                os.close(fd)
        # Each selected subject must actually answer its ioctl; an empty or
        # inaccessible sysfs inventory is never evidence of its retirement.
        for row in expected['entries']:
            check(row['number'])
        inventory = os.open('/sys/block', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            # Path.iterdir materializes scandir on some supported Pythons.
            # Consume the held-directory scandir stream directly and stop at
            # the first excess entry, without allocating a full inventory.
            with os.scandir(inventory) as entries:
                for count, entry in enumerate(entries, 1):
                    require(count <= 65536, 'native block inventory exceeds bound')
                    match = re.fullmatch(r'loop([0-9]+)', entry.name)
                    if match:
                        check(int(match[1]))
        finally:
            os.close(inventory)
    return {'schema': 1, 'observed_backings': len(wanted), 'owned_associations_retired': True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='operation', required=True)
    setup = commands.add_parser('create')
    setup.add_argument('--directory', type=Path, required=True)
    setup.add_argument('--name', choices=('a', 'b'), required=True)
    setup.add_argument('--size', required=True)
    setup.add_argument('--journal-directory', type=Path, required=True)
    observer = commands.add_parser('observe')
    observer.add_argument('--journal', type=Path, required=True)
    args = parser.parse_args()
    if args.operation == 'create':
        create(args.directory, args.name, args.size, args.journal_directory)
    else:
        print(json.dumps(observe(args.journal), sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError):
        raise SystemExit('native autoclear fixture refused')
