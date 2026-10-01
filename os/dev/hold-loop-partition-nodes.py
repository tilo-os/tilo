#!/usr/bin/env python3
"""Hold validated Kata loop partition nodes until the disk installer releases them."""

import sys

from private_disk_image import loop_partition_nodes


if len(sys.argv) not in (2, 3):
    raise SystemExit('usage: hold-loop-partition-nodes.py /dev/loopN [partition-count]')

numbers = (1, 2)
if len(sys.argv) == 3:
    try:
        count = int(sys.argv[2])
    except ValueError:
        raise SystemExit('partition-count must be an integer') from None
    if count < 1:
        raise SystemExit('partition-count must be positive')
    numbers = tuple(range(1, count + 1))

with loop_partition_nodes(sys.argv[1], numbers):
    print('READY', flush=True)
    if sys.stdin.readline() != 'release\n':
        raise SystemExit('loop partition node holder ended without release')
