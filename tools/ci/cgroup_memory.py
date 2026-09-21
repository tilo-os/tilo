#!/usr/bin/env python3
"""Best-effort cgroup-v2 memory evidence, without attributing kills to a process.

TILO_CGROUP_MEMORY_PATH may select a known cgroup directory explicitly (also
used by portable fixtures). Otherwise resolve this process's cgroup2 mount.
Missing fields remain null; historical OOM counts alone are not new evidence.
"""
import json
import os
from pathlib import Path, PurePosixPath
import re
import sys


FIELDS = ('memory.max', 'memory.current', 'memory.peak', 'memory.events')


def unescape_mount(value):
    return re.sub(r'\\([0-7]{3})', lambda match: chr(int(match[1], 8)), value)


def resolve_cgroup(cgroup, mountinfo):
    groups = [line[3:] for line in cgroup.splitlines() if line.startswith('0::')]
    if len(groups) != 1:
        return None
    group = PurePosixPath(groups[0])
    if not group.is_absolute() or '..' in group.parts:
        return None
    for line in mountinfo.splitlines():
        left, separator, right = line.partition(' - ')
        fields = left.split()
        if not separator or not right.startswith('cgroup2 ') or len(fields) < 5:
            continue
        root = PurePosixPath(unescape_mount(fields[3]))
        try:
            relative = group.relative_to(root)
        except ValueError:
            continue
        return Path(unescape_mount(fields[4])) / relative
    return None


def current_cgroup():
    override = os.environ.get('TILO_CGROUP_MEMORY_PATH')
    if override:
        return Path(override)
    try:
        return resolve_cgroup(Path('/proc/self/cgroup').read_text(),
                              Path('/proc/self/mountinfo').read_text())
    except OSError:
        return None


def snapshot():
    path = current_cgroup()
    result = dict.fromkeys(FIELDS)
    result.update(cgroup=str(path) if path is not None else None, identity=None)
    if path is None:
        return result
    try:
        stat = path.stat()
        result['identity'] = [stat.st_dev, stat.st_ino]
    except OSError:
        return result
    for name in FIELDS:
        try:
            value = (path / name).read_text().strip()
            if name == 'memory.events':
                events = {}
                for line in value.splitlines():
                    key, count = line.split()
                    if not re.fullmatch(r'[a-z_]+', key) or not re.fullmatch(r'[0-9]+', count):
                        raise ValueError('invalid event counter')
                    events[key] = int(count)
                result[name] = events or None
            elif re.fullmatch(r'[0-9]+', value) or (name == 'memory.max' and value == 'max'):
                result[name] = value
        except (OSError, ValueError):
            pass  # Unknown is not zero, and diagnostics must not mask a failure.
    return result


def report(before, after):
    print('CI_CGROUP_MEMORY_BEFORE=' + json.dumps(before, sort_keys=True))
    print('CI_CGROUP_MEMORY_AFTER=' + json.dumps(after, sort_keys=True))
    if (before.get('identity') is None or before.get('identity') != after.get('identity')
            or before.get('cgroup') != after.get('cgroup')):
        return
    old, new = before.get('memory.events'), after.get('memory.events')
    if old is None or new is None:
        return
    delta = {key: value - old[key] for key, value in new.items()
             if key in old and value > old[key]}
    if delta:
        print('CI_CGROUP_MEMORY_EVENTS_DELTA=' + json.dumps(delta, sort_keys=True))


def main():
    if len(sys.argv) == 1:
        print(json.dumps(snapshot(), sort_keys=True))
    else:
        try:
            before = json.loads(Path(sys.argv[1]).read_text())
        except (OSError, ValueError):
            before = dict.fromkeys(FIELDS)
        report(before, snapshot())


if __name__ == '__main__':
    main()
