#!/usr/bin/env python3
"""Numeric descendant-process receipt; never read command lines or environment."""
import argparse
import json
import os
from pathlib import Path


def snapshot(root_pid, proc=Path('/proc')):
    rows = {}
    for path in proc.iterdir():
        if not path.name.isdecimal():
            continue
        try:
            raw = (path / 'stat').read_text()
            # comm can contain spaces and parentheses; fields after its last ')'
            # start at Linux proc_pid_stat field 3 (state).
            fields = raw[raw.rindex(')') + 2:].split()
            comm = raw[raw.index('(') + 1:raw.rindex(')')]
            kind = {'rustc': 'compiler', 'cc': 'link-driver', 'clang': 'link-driver',
                    'ld': 'linker', 'ld.lld': 'linker', 'rust-lld': 'linker',
                    'mold': 'linker', 'cargo': 'cargo', 'cargo-nextest': 'nextest'}.get(comm, 'other')
            rows[int(path.name)] = {
                'pid': int(path.name), 'ppid': int(fields[1]),
                'kind': kind,
                'state': fields[0], 'cpu_ticks': int(fields[11]) + int(fields[12]),
                'start_ticks': int(fields[19]),
                'rss_pages': int(fields[21]),
            }
        except (OSError, ValueError, IndexError):
            continue  # A process may exit while procfs is being read.
    if root_pid not in rows:
        raise RuntimeError('root process is unavailable')
    descendants = {root_pid}
    while True:
        found = {pid for pid, row in rows.items() if row['ppid'] in descendants}
        expanded = descendants | found
        if expanded == descendants:
            break
        descendants = expanded
    selected = [row for pid, row in rows.items()
                if pid in descendants and pid not in (root_pid, os.getpid())]
    for row in selected:
        try:
            counters = dict(line.split(':', 1) for line in
                            (proc / str(row['pid']) / 'io').read_text().splitlines())
            row['read_bytes'] = int(counters['read_bytes'])
            row['write_bytes'] = int(counters['write_bytes'])
        except (OSError, KeyError, ValueError):
            pass  # Missing counters remain unknown, never reported as zero.
    selected.sort(key=lambda row: (-row['cpu_ticks'], row['pid']))
    return {'clock_ticks_per_second': os.sysconf('SC_CLK_TCK'),
            'page_size_bytes': os.sysconf('SC_PAGE_SIZE'),
            'descendant_count': len(selected), 'processes': selected[:16],
            'truncated': len(selected) > 16}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root-pid', type=int, required=True)
    args = parser.parse_args()
    print('CI_PROCESS_PROGRESS=' + json.dumps(snapshot(args.root_pid), separators=(',', ':')))


if __name__ == '__main__':
    main()
