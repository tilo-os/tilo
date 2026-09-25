#!/usr/bin/env python3
"""Hold validated Kata loop partition nodes until the disk installer releases them."""

import sys

from private_disk_image import loop_partition_nodes


if len(sys.argv) != 2:
    raise SystemExit('usage: hold-loop-partition-nodes.py /dev/loopN')

with loop_partition_nodes(sys.argv[1]):
    print('READY', flush=True)
    if sys.stdin.readline() != 'release\n':
        raise SystemExit('loop partition node holder ended without release')
