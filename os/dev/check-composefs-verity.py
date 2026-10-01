#!/usr/bin/env python3
"""Require fs-verity on every object of an installed composefs repository.

    check-composefs-verity.py OBJECTS_DIR

A repository whose objects carry no fs-verity boots, but its first `bootc
upgrade` fails with EIO ("lower file ... has no fs-verity digest"). bootc
installs such a repository silently when the installing kernel lacks fs-verity.
"""
import fcntl
import os
import struct
import sys

FS_IOC_MEASURE_VERITY = 0xC0046686


def has_verity(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        fcntl.ioctl(fd, FS_IOC_MEASURE_VERITY, bytearray(struct.pack("HH", 0, 64)) + bytearray(64))
        return True
    except OSError:
        return False
    finally:
        os.close(fd)


def main():
    total = missing = 0
    for directory, _, names in os.walk(sys.argv[1]):
        for name in names:
            total += 1
            missing += not has_verity(os.path.join(directory, name))
    print(f"composefs objects with fs-verity: {total - missing} of {total}")
    return 1 if missing or not total else 0


if __name__ == "__main__":
    sys.exit(main())
