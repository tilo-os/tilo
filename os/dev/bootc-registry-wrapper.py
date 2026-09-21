#!/usr/bin/env python3
"""Warm the exact registry path, then supervise bootc without a shell."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import shutil
import tempfile
import threading
import time
import tomllib

import installer_bootc_helper


SOURCE = re.compile(r'docker://code\.podesta\.ai/[a-z0-9_./-]+@(sha256:[0-9a-f]{64})')
INSTALLER_BOOTC = '/run/tilo-installer-bootc'


class ForwardedSignal(Exception):
    def __init__(self, signum):
        super().__init__(signum)
        self.signum = signum


def _private_directory(path):
    info = path.lstat()
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_gid != 0
            or stat.S_IMODE(info.st_mode) != 0o700):
        raise ValueError(f'{path} is not a root-owned private directory')


@contextmanager
def private_source_store_view(lower=Path('/run/tilo-parent-containers'),
                              target=Path('/var/lib/containers'),
                              scratch=Path('/run/tilo-source-store-scratch')):
    """Mount a private COW store after the parent created this container."""
    lower_info = lower.lstat()
    if (not stat.S_ISDIR(lower_info.st_mode) or lower_info.st_uid != 0
            or lower_info.st_gid != 0 or stat.S_IMODE(lower_info.st_mode) & 0o022
            or not os.statvfs(lower).f_flag & os.ST_RDONLY):
        raise ValueError('parent source store is not a protected read-only directory')
    _private_directory(scratch)
    created_target = False
    try:
        target_info = target.lstat()
    except FileNotFoundError:
        parent_info = target.parent.lstat()
        if (not stat.S_ISDIR(parent_info.st_mode) or parent_info.st_uid != 0
                or parent_info.st_gid != 0
                or stat.S_IMODE(parent_info.st_mode) not in (0o755, 0o775)):
            raise ValueError('private source store parent is not protected')
        target.mkdir(mode=0o700)
        created_target = True
        target_info = target.lstat()
    if (not stat.S_ISDIR(target_info.st_mode) or target_info.st_uid != 0
            or target_info.st_gid != 0 or stat.S_IMODE(target_info.st_mode) & 0o022):
        raise ValueError('private source store mountpoint is not protected')
    original = (target_info.st_dev, target_info.st_ino)
    root = Path(tempfile.mkdtemp(prefix='tilo-source-store-', dir=scratch))
    os.chmod(root, 0o700)
    upper, work = (root / name for name in ('upper', 'work'))
    for path in (upper, work):
        path.mkdir(mode=0o700)
        _private_directory(path)
    process = None
    mounted = False
    cleanup_error = None
    try:
        process = subprocess.Popen(
            ['/usr/bin/fuse-overlayfs', '-f', '-o',
             f'lowerdir={lower},upperdir={upper},workdir={work}', str(target)],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
        for _ in range(100):
            if process.poll() is not None:
                raise ValueError('private source store view exited before mounting')
            result = subprocess.run(
                ['/usr/bin/findmnt', '-n', '-o', 'FSTYPE', '-T', str(target)],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
            if result.returncode == 0 and result.stdout.strip() == b'fuse.fuse-overlayfs':
                mounted = True
                break
            time.sleep(0.1)
        if not mounted:
            raise ValueError('private source store view did not mount')
        yield
    finally:
        if mounted:
            result = subprocess.run(
                ['/usr/bin/fusermount3', '-u', str(target)], stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
            if result.returncode:
                result = subprocess.run(
                    ['/usr/bin/fusermount3', '-uz', str(target)], stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
                if result.returncode:
                    cleanup_error = ValueError('private source store view did not unmount')
        if process is not None:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(timeout=10)
                cleanup_error = cleanup_error or ValueError(
                    'private source store view did not stop after unmount')
        try:
            shutil.rmtree(root)
        except OSError as error:
            cleanup_error = cleanup_error or error
        if created_target:
            try:
                target.rmdir()
            except OSError as error:
                cleanup_error = cleanup_error or error
            if target.exists():
                cleanup_error = cleanup_error or ValueError(
                    'private source store mountpoint survived cleanup')
        else:
            current = target.lstat()
            if (current.st_dev, current.st_ino) != original:
                cleanup_error = cleanup_error or ValueError(
                    'private source store view survived cleanup')
        if cleanup_error is not None:
            raise cleanup_error


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
    try:
        with open('/run/.containerenv', 'rb') as stream:
            fields = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ValueError('container identity metadata is ambiguous') from error
    if not isinstance(fields, dict):
        raise ValueError('container identity metadata is ambiguous')
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


def require_install_target(argv, expected_source, mountinfo=Path('/proc/self/mountinfo')):
    """Fail closed unless bootc sees both prepared filesystems recursively."""
    if argv[1:3] != ['install', 'to-filesystem']:
        return
    sources = [item for item in argv if item.startswith('--source-imgref=')]
    transports = [item for item in argv if item.startswith('--target-transport=')]
    if (argv[-1:] != ['/target'] or argv.count('--root-mount-spec=') != 1
            or sources != ['--source-imgref=' + expected_source]
            or transports != ['--target-transport=registry']):
        raise ValueError('to-filesystem command must use the prepared target exactly')
    observed = {}
    for raw in mountinfo.read_text().splitlines():
        fields = raw.split(' - ')
        left = fields[0].split() if len(fields) == 2 else []
        right = fields[1].split() if len(fields) == 2 else []
        if len(left) < 6 or len(right) < 3:
            raise ValueError('install target mount inventory is malformed')
        target = left[4]
        if target in ('/target', '/target/boot'):
            if target in observed:
                raise ValueError('install target mount inventory is ambiguous')
            observed[target] = (left[2], right[0])
    expected = {'/target': 'btrfs', '/target/boot': 'vfat'}
    if ({target: identity[1] for target, identity in observed.items()} != expected
            or observed['/target'][0] == observed['/target/boot'][0]):
        raise ValueError('prepared root and ESP are not recursively visible')


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
    if not argv or argv[0] != INSTALLER_BOOTC or heartbeat_interval <= 0:
        raise ValueError('invalid supervised bootc command')
    require_bound_source(source, expected)
    require_install_target(argv, source)
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
        raise ForwardedSignal(forwarded[0])
    return returncode


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--installer-bootc-sha256', required=True)
    parser.add_argument('--installer-bootc-architecture', required=True)
    parser.add_argument('--installer-bootc-runtime-receipt', required=True, type=Path)
    parser.add_argument('source')
    parser.add_argument('expected_digest')
    parser.add_argument('bootc', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    argv = args.bootc[1:] if args.bootc[:1] == ['--'] else args.bootc
    require_private_udev()
    identity = installer_bootc_helper.verify_bound_binary(
        INSTALLER_BOOTC, args.installer_bootc_sha256,
        args.installer_bootc_architecture)
    mount = installer_bootc_helper.mountinfo_receipt(INSTALLER_BOOTC)
    installer_bootc_helper.write_runtime_receipt(
        args.installer_bootc_runtime_receipt, identity, mount)
    try:
        with private_source_store_view():
            return run_bootc(args.source, args.expected_digest, argv)
    except ForwardedSignal as forwarded:
        signal.signal(forwarded.signum, signal.SIG_DFL)
        os.kill(os.getpid(), forwarded.signum)
        raise AssertionError('signal propagation returned')


if __name__ == '__main__':
    raise SystemExit(main())
