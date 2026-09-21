#!/usr/bin/env python3
"""Warm the exact registry path, then supervise bootc without a shell."""
import argparse
import hashlib
import json
import os
import re
import signal
import stat
import subprocess
import threading
import time


SOURCE = re.compile(r'docker://code\.podesta\.ai/[a-z0-9_./-]+@(sha256:[0-9a-f]{64})')


def require_private_udev():
    current = os.stat('/run/udev', follow_symlinks=False)
    pid1 = os.stat('/proc/1/root/run/udev', follow_symlinks=False)
    if not stat.S_ISDIR(current.st_mode) or not stat.S_ISDIR(pid1.st_mode):
        raise ValueError('supervised udev namespace is not a directory')
    if current.st_dev != pid1.st_dev:
        raise ValueError('supervised udev namespace differs from PID 1')
    if not os.statvfs('/run/udev').f_flag & os.ST_RDONLY:
        raise ValueError('supervised udev namespace is writable')
    if os.path.lexists('/run/udev/data'):
        raise ValueError('supervised udev namespace contains a stale database')


def require_bound_source(source, expected):
    match = SOURCE.fullmatch(source)
    if match is None or match.group(1) != expected:
        raise ValueError('bound source check requires the exact immutable source')
    fields = {}
    for line in open('/run/.containerenv', encoding='utf-8'):
        item = re.fullmatch(r'([a-z]+)="([^"\n]*)"\n?', line)
        if item is None or item.group(1) in fields:
            raise ValueError('container identity metadata is ambiguous')
        fields[item.group(1)] = item.group(2)
    image_id = fields.get('imageid', '').removeprefix('sha256:')
    canonical = source.removeprefix('docker://')
    if (not re.fullmatch(r'[0-9a-f]{64}', image_id)
            or fields.get('image') != canonical):
        raise ValueError('running container does not match the immutable source')
    inspect = subprocess.run(
        ['/usr/bin/podman', '--remote=false', 'image', 'inspect', image_id],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
    if inspect.returncode:
        raise ValueError('private source store does not contain the running image')
    try:
        rows = json.loads(inspect.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError('private source store returned invalid image metadata') from error
    if not isinstance(rows, list) or len(rows) != 1:
        raise ValueError('private source store returned ambiguous image metadata')
    row = rows[0]
    if (row.get('Id', '').removeprefix('sha256:') != image_id
            or row.get('Digest') != expected or row.get('RepoDigests') != [canonical]):
        raise ValueError('private source store image identity mismatch')
    raw = subprocess.run(
        ['/usr/bin/skopeo', 'inspect', '--raw', 'containers-storage:' + canonical],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
    if raw.returncode or 'sha256:' + hashlib.sha256(raw.stdout).hexdigest() != expected:
        raise ValueError('private source store manifest identity mismatch')
    config = subprocess.run(
        ['/usr/bin/skopeo', 'inspect', 'containers-storage:' + canonical],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
    try:
        document = json.loads(config.stdout) if not config.returncode else None
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError('private source store returned invalid config metadata') from error
    if (not isinstance(document, dict) or document.get('Digest') != expected
            or document.get('Architecture') != 'arm64'):
        raise ValueError('private source store config identity mismatch')


def inspect_digest(source, expected, *, timeout=20):
    match = SOURCE.fullmatch(source)
    if match is None or match.group(1) != expected:
        raise ValueError('registry warmup requires the exact immutable source')
    auth = os.environ.get('REGISTRY_AUTH_FILE')
    if not auth:
        raise ValueError('registry warmup requires scoped authentication')
    result = subprocess.run(
        ['/usr/bin/skopeo', 'inspect', '--format', '{{.Digest}}',
         '--authfile', auth, source], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        timeout=timeout, env={'PATH': '/usr/bin:/bin', 'HOME': '/root',
                              'LANG': 'C.UTF-8', 'REGISTRY_AUTH_FILE': auth})
    return result.returncode == 0 and result.stdout.strip() == expected.encode()


def warm(source, expected, *, attempts=3, delays=(2, 5), sleep=time.sleep):
    if attempts != 3 or tuple(delays) != (2, 5):
        raise ValueError('registry warmup policy is fixed')
    for attempt in range(attempts):
        try:
            if inspect_digest(source, expected):
                return True
        except (OSError, subprocess.SubprocessError):
            pass
        if attempt + 1 < attempts:
            sleep(delays[attempt])
    return False


def run_bootc(source, expected, argv, *, heartbeat_interval=30):
    if not argv or argv[0] != '/usr/bin/bootc' or heartbeat_interval <= 0:
        raise ValueError('invalid supervised bootc command')
    require_bound_source(source, expected)
    if not warm(source, expected):
        return 75

    stop = threading.Event()

    def heartbeat():
        while not stop.wait(heartbeat_interval):
            try:
                inspect_digest(source, expected, timeout=10)
            except (OSError, subprocess.SubprocessError):
                pass

    thread = threading.Thread(target=heartbeat, name='tilo-registry-heartbeat')
    thread.start()
    child = None
    forwarded = []
    previous = {}

    def forward(signum, _frame):
        if not forwarded:
            forwarded.append(signum)
        if child is not None and child.poll() is None:
            child.send_signal(signum)

    try:
        for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            previous[signum] = signal.signal(signum, forward)
        child = subprocess.Popen(argv)
        returncode = child.wait()
    finally:
        stop.set()
        thread.join(timeout=12)
        if thread.is_alive():
            raise RuntimeError('registry heartbeat did not stop')
        for signum, handler in previous.items():
            signal.signal(signum, handler)
    if forwarded:
        signal.signal(forwarded[0], signal.SIG_DFL)
        os.kill(os.getpid(), forwarded[0])
        raise AssertionError('signal propagation returned')
    return returncode


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('source')
    parser.add_argument('expected_digest')
    parser.add_argument('bootc', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    argv = args.bootc[1:] if args.bootc[:1] == ['--'] else args.bootc
    require_private_udev()
    return run_bootc(args.source, args.expected_digest, argv)


if __name__ == '__main__':
    raise SystemExit(main())
