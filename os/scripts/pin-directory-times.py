#!/usr/bin/env python3
"""Set the mtime of every directory on ROOT's filesystem to EPOCH.

Only directories of that filesystem: mount points below it (/proc, /sys, /dev
in a build container) belong to other filesystems and are neither changed nor
entered. Symlinks are not followed.
"""

import os
import sys


def pin(root: str, epoch: int) -> int:
    device = os.lstat(root).st_dev
    count = 0
    for directory, subdirectories, _ in os.walk(root):
        subdirectories[:] = [
            name for name in subdirectories
            if os.lstat(os.path.join(directory, name)).st_dev == device
        ]
        os.utime(directory, (epoch, epoch), follow_symlinks=False)
        count += 1
    return count


if __name__ == "__main__":
    root, epoch = sys.argv[1], int(sys.argv[2])
    print(f"pinned {pin(root, epoch)} directories under {root} to {epoch}")
