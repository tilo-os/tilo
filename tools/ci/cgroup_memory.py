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
import time


FIELDS = ('memory.max', 'memory.current', 'memory.peak', 'memory.events')
CPU_COUNTERS = ('usage_usec', 'user_usec', 'system_usec', 'nr_periods', 'nr_throttled', 'throttled_usec')


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


def resource_snapshot():
    """Use the same observed cgroup identity for successful command evidence."""
    result = snapshot()
    result['monotonic_ns'] = time.monotonic_ns()
    for name in ('cpu.max', 'cpuset.cpus.effective', 'cpu.stat',
                 'cpu.pressure', 'memory.pressure', 'io.pressure'):
        result[name] = None
        if result['cgroup'] is not None:
            try:
                value = (Path(result['cgroup']) / name).read_text()
                if len(value) <= 8192:
                    result[name] = value.strip()
            except OSError:
                pass
    return result


def numeric_stat(value):
    if not isinstance(value, str):
        return 'unavailable'
    values = {}
    for line in value.splitlines():
        fields = line.split()
        if len(fields) != 2:
            return 'unavailable'
        key, count = fields
        if key in CPU_COUNTERS:
            if not re.fullmatch(r'[0-9]{1,20}', count) or key in values:
                return 'unavailable'
            values[key] = int(count)
    return ' '.join(f'{key} {values[key]}' for key in CPU_COUNTERS if key in values) or 'unavailable'


def resource_report(before, after, label):
    # Emit the existing measure.sh format, without raw paths or child commands.
    # CPU deltas are valid only while the observed cgroup identity is unchanged.
    same = (before.get('identity') is not None and before.get('identity') == after.get('identity')
            and before.get('cgroup') == after.get('cgroup'))
    started, stopped = before.get('monotonic_ns'), after.get('monotonic_ns')
    wall = (round((stopped - started) / 1e9, 6)
            if type(started) is int and type(stopped) is int and stopped >= started else 'unavailable')
    print(f'==> {label} measurements')
    print(f'    wall_seconds={wall}')
    for key in ('user_seconds', 'system_seconds', 'max_rss_kib'):
        print(f'    {key}=unavailable')
    quota = after.get('cpu.max')
    quota = quota if isinstance(quota, str) and re.fullmatch(r'(?:max|[0-9]{1,20}) [0-9]{1,20}', quota) else 'unavailable'
    print(f'    cgroup_cpu_max={quota}')
    cpus = after.get('cpuset.cpus.effective')
    cpus = cpus if isinstance(cpus, str) and re.fullmatch(r'[0-9,-]{1,8192}', cpus) else 'unavailable'
    print(f'    cgroup_cpuset_cpus_effective={cpus}')
    print(f'    cgroup_cpu_stat_before={numeric_stat(before.get("cpu.stat")) if same else "unavailable"}')
    print(f'    cgroup_cpu_stat_after={numeric_stat(after.get("cpu.stat"))}')
    for resource in ('cpu', 'memory', 'io'):
        for phase, reading in (('before', before), ('after', after)):
            value = reading.get(resource + '.pressure')
            # Preserve only the known numeric PSI rows; never arbitrary text.
            rows = []
            if isinstance(value, str) and (phase != 'before' or same):
                for line in value.splitlines():
                    if re.fullmatch(r'(?:some|full) avg10=[0-9.]+ avg60=[0-9.]+ avg300=[0-9.]+ total=[0-9]{1,20}', line):
                        rows.append(line)
            value = ' '.join(rows) if rows else 'unavailable'
            print(f'    cgroup_{resource}_pressure_{phase}={value}')
    for name in ('memory.max', 'memory.peak'):
        value = after.get(name) or 'unavailable'
        print(f'    cgroup_{name.replace(".", "_")}={value}')


def main():
    if sys.argv[1:2] == ['resource-before']:
        print(json.dumps(resource_snapshot(), sort_keys=True))
    elif sys.argv[1:2] == ['resource-after'] and len(sys.argv) == 4:
        try:
            before = json.loads(Path(sys.argv[2]).read_text())
        except (OSError, ValueError):
            before = {}
        resource_report(before, resource_snapshot(), sys.argv[3])
    elif len(sys.argv) == 1:
        print(json.dumps(snapshot(), sort_keys=True))
    else:
        try:
            before = json.loads(Path(sys.argv[1]).read_text())
        except (OSError, ValueError):
            before = dict.fromkeys(FIELDS)
        report(before, snapshot())


if __name__ == '__main__':
    main()
