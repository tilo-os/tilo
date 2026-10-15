#!/usr/bin/env python3
"""Private VM fixture only: format serial-bound, wholly zero virtio disks.

The provenance file is supplied by disposable-native-vm.py, not discovered from
arbitrary guest disks. This does not relax any production backing checks.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess


def require(ok, message):
    if not ok:
        raise ValueError(message)


def provenance(directory, path):
    require(os.geteuid() == 0, 'requires root in the private guest')
    require(directory.is_absolute() and directory.resolve() == directory,
            'fixture path must not redirect')
    for parent in (directory, *directory.parents):
        info = parent.stat()
        require(info.st_uid == 0 and (not info.st_mode & 0o022
                or parent == Path('/var/tmp')), 'fixture ancestors must be root controlled')
    require(stat.S_IMODE(directory.stat().st_mode) == 0o700, 'fixture must be private')
    require(path == directory / 'provenance.json', 'provenance must be inside fixture')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd) as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
                and stat.S_IMODE(info.st_mode) == 0o600 and info.st_size < 4096,
                'provenance is not a protected regular file')
        value = json.load(stream)
    require(value['schema'] == 1 and re.fullmatch('native-store-private-[0-9a-f]{16}', value['vm'])
            and re.fullmatch('[0-9a-f]{64}', value['base_sha256']), 'invalid fixture identity')
    rows = value['disks']
    require(len(rows) == 2 and {row['name'] for row in rows} == {'a', 'b'}
            and len({row['serial'] for row in rows}) == 2, 'two distinct stores required')
    for row in rows:
        require(row['serial'] == f"tns-{value['vm'].removeprefix('native-store-private-')[:12]}-{row['name']}"
                and row['size'] == 1024 ** 3, 'invalid auxiliary disk identity')
    return rows


def device(row):
    matches = [p for p in Path('/sys/class/block').glob('vd*')
               if (p / 'serial').exists() and (p / 'serial').read_text().strip() == row['serial']]
    require(len(matches) == 1, 'expected exactly one serial-matching virtio disk')
    sys = matches[0]
    require(not (sys / 'partition').exists() and not list((sys / 'holders').iterdir())
            and not list(sys.glob(sys.name + '*')), 'disk has partitions or holders')
    number = sys.joinpath('dev').read_text().strip()
    major, minor = map(int, number.split(':'))
    require(sys.joinpath('size').read_text().strip() == str(row['size'] // 512), 'disk size differs')
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        fields = line.split()
        source = fields[fields.index('-') + 2]
        source_number = os.stat(source).st_rdev if source.startswith('/dev/') else None
        require(fields[2] != number and source_number != os.makedev(major, minor),
                'disk is mounted (including root)')
    swaps = Path('/proc/swaps').read_text().splitlines()[1:]
    require(all(os.stat(line.split()[0]).st_rdev != os.makedev(major, minor)
                for line in swaps), 'disk is swap')
    return Path('/dev') / sys.name, os.makedev(major, minor)


def run(*args, **kwargs):
    subprocess.run(args, check=True, **kwargs)


def require_zero(fd, size):
    remaining = size
    while remaining:
        chunk = os.read(fd, min(4 * 1024 ** 2, remaining))
        require(chunk and not any(chunk), 'qualification disk is not wholly zero')
        remaining -= len(chunk)


def up(directory, rows):
    # Refuse reuse, including a previous failed attempt. Never reformat a store.
    marker = directory / 'format-started'
    fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    os.close(fd)
    mounted = []
    try:
        for row in rows:
            path, number = device(row)
            # O_EXCL on a block device is the kernel's claim, not an advisory flock.
            fd = os.open(path, os.O_RDWR | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC)
            try:
                require(stat.S_ISBLK(os.fstat(fd).st_mode) and os.fstat(fd).st_rdev == number,
                        'opened disk identity differs')
                size = struct.unpack('Q', fcntl.ioctl(fd, 0x80081272, bytes(8)))[0]
                require(size == row['size'], 'kernel disk size differs')
                require_zero(fd, size)
                require(device(row) == (path, number), 'disk changed during zero check')
                # mkfs.btrfs reopens its argument with O_EXCL: even a proc-fd
                # name cannot share our kernel claim. Format a private regular
                # image, then install its bytes through the still-held claim.
                image = directory / (row['name'] + '.format')
                image_fd = os.open(image, os.O_RDWR | os.O_CREAT | os.O_EXCL
                                   | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
                try:
                    os.ftruncate(image_fd, size)
                    run('mkfs.btrfs', '-q', str(image))
                    os.lseek(fd, 0, os.SEEK_SET)
                    while chunk := os.read(image_fd, 1024 ** 2):
                        if any(chunk):
                            view = memoryview(chunk)
                            while view:
                                written = os.write(fd, view)
                                require(written > 0, 'short format write')
                                view = view[written:]
                        else:
                            os.lseek(fd, len(chunk), os.SEEK_CUR)
                    os.fsync(fd)
                finally:
                    os.close(image_fd)
                    image.unlink()
            finally:
                os.close(fd)
            # Mounting itself needs a kernel claim, so release the format claim first.
            require(device(row) == (path, number), 'disk changed before mounting')
            mount = directory / row['name']
            mount.mkdir(mode=0o700)
            run('mount', '-t', 'btrfs', '-o', 'subvolid=5', str(path), str(mount))
            mounted.append(mount)
    except BaseException:
        for mount in reversed(mounted):
            run('umount', str(mount))
        raise
    print(f'TILO_FS_NATIVE_STORES={directory}/a:{directory}/b')
    print(f'TILO_FS_NATIVE_STORE={directory}/a')
    print('TILO_REQUIRE_FANOTIFY=1')


def down(directory, rows):
    # Only unmount this fixture's exact serial-bound devices. Never detach loops.
    for row in rows:
        mount = directory / row['name']
        matches = [line.split() for line in Path('/proc/self/mountinfo').read_text().splitlines()
                   if line.split()[4] == str(mount)]
        if matches:
            serial = row['serial']
            disks = [p for p in Path('/sys/class/block').glob('vd*')
                     if (p / 'serial').exists() and (p / 'serial').read_text().strip() == serial]
            require(len(matches) == len(disks) == 1, 'fixture serial or mount is ambiguous')
            fields = matches[0]
            separator = fields.index('-')
            major, minor = map(int, (disks[0] / 'dev').read_text().strip().split(':'))
            source = fields[separator + 2]
            require(fields[separator + 1] == 'btrfs' and fields[3] == '/'
                    and source.startswith('/dev/')
                    and os.stat(source).st_rdev == os.makedev(major, minor),
                    'refusing to unmount a substituted fixture')
            run('umount', str(mount))
        if mount.exists():
            mount.rmdir()


def observe(directory, rows):
    require((directory / 'format-started').is_file(), 'actual store attempt was not observed')
    for row in rows:
        device(row)  # Includes mounted/root/swap rejection on the actual block device.
        require(not (directory / row['name']).exists(), 'owned mount endpoint not retired')
    print(json.dumps({'schema': 1, 'observed_disks': len(rows), 'owned_mounts_retired': True}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('up', 'down', 'observe'))
    parser.add_argument('directory', type=Path)
    parser.add_argument('provenance', type=Path)
    args = parser.parse_args()
    rows = provenance(args.directory, args.provenance)
    {'up': up, 'down': down, 'observe': observe}[args.action](args.directory, rows)


if __name__ == '__main__':
    main()
