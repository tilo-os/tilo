#!/usr/bin/env python3
"""Hold validated Kata loop partition nodes until the disk installer releases them."""

import sys

from private_disk_image import loop_partition_nodes


if len(sys.argv) not in (2, 3, 4):
    raise SystemExit('usage: hold-loop-partition-nodes.py /dev/loopN [partition-count] [timeout]')

numbers = (1, 2)
timeout = 5.0
if len(sys.argv) >= 3:
    try:
        count = int(sys.argv[2])
    except ValueError:
        raise SystemExit('partition-count must be an integer') from None
    if count < 1:
        raise SystemExit('partition-count must be positive')
    numbers = tuple(range(1, count + 1))
if len(sys.argv) == 4:
    # A caller that starts the holder before the partitions exist (the LUKS
    # install layout is written by the installer after this) waits for them.
    try:
        timeout = float(sys.argv[3])
    except ValueError:
        raise SystemExit('timeout must be a number of seconds') from None
    if timeout <= 0:
        raise SystemExit('timeout must be positive')

with loop_partition_nodes(sys.argv[1], numbers, timeout=timeout):
    print('READY', flush=True)
    if sys.stdin.readline() != 'release\n':
        raise SystemExit('loop partition node holder ended without release')
