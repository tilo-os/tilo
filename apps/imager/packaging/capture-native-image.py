#!/usr/bin/env python3
"""Execute an owned native namespace from a rootless-exported qualification image.

Podman only owns a stopped rootless filesystem container. The existing native
fixture owns loop/fanotify work in a fresh privileged mount/PID namespace; no
rootful Podman storage, daemon or container execution is involved.
"""
import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import subprocess
import sys
import tempfile
import uuid


def checked(command, **kwargs):
    return subprocess.run(list(map(str, command)), check=True, **kwargs)


def bind(source, destination, readonly=False):
    if source.is_dir():
        destination.mkdir(parents=True, exist_ok=True)
    else:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.touch(exist_ok=True)
    checked(['mount', '--bind', source, destination], stdout=subprocess.DEVNULL)
    if readonly:
        checked(['mount', '-o', 'remount,bind,ro', destination], stdout=subprocess.DEVNULL)


def protect_runtime_trust(root):
    """Keep installed trust immutable without freezing generated capability facts."""
    for relative in ('usr/share/tilo/runtime-bundle-publisher.hex',
                     'usr/share/tilo/browser-runtime-publisher.hex',
                     'usr/share/tilo/registry-runtime-publisher.hex',
                     'usr/share/tilo/runtime-retained'):
        path = root / relative
        if path.exists():
            bind(path, path, True)


def admit_native_stores(stage, provenance):
    """Validate the owned real-disk contract before exposing it across pivot."""
    if (provenance is None or not provenance.is_absolute()
            or provenance.parent.parent != Path('/var/tmp')
            or not re.fullmatch('native-store-private-[0-9a-f]{16}', provenance.parent.name)):
        raise ValueError('explicit owned virtio provenance required; loops cannot qualify')
    spec = importlib.util.spec_from_file_location('owned_virtio', stage / 'native-store-virtio.py')
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    rows = helper.provenance(provenance.parent, provenance)
    for row in rows:
        helper.device(row)
    return provenance.parent


def native(args):
    if os.getuid() != 0 or os.geteuid() != 0:
        raise ValueError('native namespace requires the authorized operator')
    if not args.child:
        with tempfile.TemporaryDirectory(prefix='tilo-native-image-', dir='/var/tmp') as scratch:
            # Keep the outer custody directory 0700, but expose an ordinary
            # 0755 '/' inside chroot so real login users can execute the OS.
            root = Path(scratch) / 'rootfs'
            root.mkdir(mode=0o755)
            checked(['tar', '--numeric-owner', '-xf', args.archive, '-C', root], stdout=subprocess.DEVNULL)
            root.chmod(0o755)
            witness = os.open('/proc/self/ns/pid', os.O_RDONLY | os.O_CLOEXEC)
            try:
                command = [sys.executable, '-B', __file__, '--child', '--native-root', root,
                           '--stage', args.stage, '--evidence', args.evidence,
                           '--pidns-fd', str(witness), '--mode', args.mode,
                           '--native-store-provenance', args.native_store_provenance]
                if args.case:
                    command += ['--case', args.case]
                child = subprocess.Popen(list(map(str, ['unshare', '--mount', '--pid', '--net',
                    '--uts', '--ipc', '--fork', '--kill-child', *command])),
                    pass_fds=(witness,), start_new_session=True)
                try:
                    # The unprivileged caller owns the write end. Its death or
                    # cancellation closes it even though it cannot signal root.
                    with selectors.DefaultSelector() as control:
                        control.register(sys.stdin, selectors.EVENT_READ)
                        while child.poll() is None:
                            if control.select(.2) and not os.read(sys.stdin.fileno(), 1):
                                if child.poll() is None:
                                    descendants = Path('/proc') / str(child.pid) / 'task' / str(child.pid) / 'children'
                                    for identifier in descendants.read_text().split():
                                        descriptor = os.pidfd_open(int(identifier), 0)
                                        try:
                                            status = (Path('/proc') / identifier / 'status').read_text()
                                            parent = [line.split()[1] for line in status.splitlines()
                                                      if line.startswith('PPid:')]
                                            if parent != [str(child.pid)]:
                                                raise ValueError('native cancellation subject changed owner')
                                            signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                                        finally:
                                            os.close(descriptor)
                                    try:
                                        child.wait(timeout=30)
                                    except subprocess.TimeoutExpired:
                                        os.killpg(child.pid, signal.SIGKILL)
                                        child.wait(timeout=10)
                                    raise ValueError('native caller cancelled; owned namespace retired')
                    if child.returncode:
                        raise subprocess.CalledProcessError(child.returncode, child.args)
                finally:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGKILL)
                        child.wait(timeout=10)
            finally:
                os.close(witness)
        return
    if os.getpid() != 1:
        raise ValueError('native subject must own its fresh PID namespace')
    root = args.native_root
    checked(['mount', '--make-rprivate', '/'], stdout=subprocess.DEVNULL)
    checked(['ip', 'link', 'set', 'lo', 'up'], stdout=subprocess.DEVNULL)
    checked(['mount', '--bind', root, root], stdout=subprocess.DEVNULL)
    stores = admit_native_stores(args.stage, args.native_store_provenance)
    bind(stores, root / str(stores).lstrip('/'))
    bind(args.native_store_provenance,
         root / str(args.native_store_provenance).lstrip('/'), True)
    bind(args.stage, root / 'qualified', True)
    bind(args.evidence, root / 'evidence')
    # mount(8) closes inherited FDs, so name this process's held witness,
    # not mount's /proc/self (the proc mount still names the outer PID IDs).
    bind(Path('/proc/self').resolve() / 'fd' / str(args.pidns_fd),
         root / 'qualification-initial-pidns', True)
    bind(Path('/dev'), root / 'dev')
    bind(Path('/sys'), root / 'sys', True)
    (root / 'proc').mkdir(exist_ok=True)
    checked(['mount', '-t', 'proc', 'proc', root / 'proc'], stdout=subprocess.DEVNULL)
    for name in ('dev/shm', 'run', 'tmp'):
        (root / name).mkdir(parents=True, exist_ok=True)
        checked(['mount', '-t', 'tmpfs', '-o', 'mode=1777' if name != 'run' else 'mode=0755',
                 'tmpfs', root / name], stdout=subprocess.DEVNULL)
    manifest = args.stage / 'source-assets.json'
    if manifest.exists():
        assets = json.loads(manifest.read_text())
        paths = {Path(row['source_root']) for row in assets['assets'].values()}
        paths.update(Path(row['source_path']) for row in assets['archives'].values())
        for path in sorted(paths):
            bind(path, root / str(path).lstrip('/'), True)
    # Exporting a bootc-like filesystem does not retain its read-only mounts.
    # Restore immutable installed publisher/retention custody before any CLI or
    # app starts; never relax the runtime's real fstatvfs/ownership checks.
    protect_runtime_trust(root)
    spec = importlib.util.spec_from_file_location('native_qualifier', args.stage / 'apps/imager/packaging/qualify-capture-release.py')
    qualifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(qualifier)
    if args.mode == 'list':
        command = ['/qualified/backend-tests', '--list']
    elif args.mode == 'observe':
        command = ['/usr/bin/python3', '-I', '/qualified/native-store-virtio.py', 'observe',
                   str(stores), str(args.native_store_provenance)]
    else:
        command = ['/usr/bin/python3', '-I', '-c', qualifier.CONTAINER_RUNNER]
    # A mere chroot makes CLONE_NEWUSER fail with EPERM, disabling CEF's
    # native sandbox. Pivot the owned mount namespace instead; never add a
    # --no-sandbox workaround or expose the operator's old root to an app.
    old = root / '.qualification-old-root'
    old.mkdir()
    os.chdir(root)
    libc = ctypes.CDLL(None, use_errno=True)
    number = {'x86_64': 155, 'aarch64': 41}[platform.machine()]
    if libc.syscall(number, b'.', b'.qualification-old-root') != 0:
        raise OSError(ctypes.get_errno(), 'owned native pivot_root failed')
    os.chdir('/')
    if libc.umount2(b'/.qualification-old-root', 2) != 0:
        raise OSError(ctypes.get_errno(), 'owned old root retirement failed')
    os.rmdir('/.qualification-old-root')
    os.execve(command[0], command, {'PATH': '/usr/bin:/usr/sbin', 'LANG': 'C', 'LC_ALL': 'C',
                                   'TILO_CAPTURE_EXACT_CASE': args.case or '',
                                   'TILO_NATIVE_STORE_VIRTIO_PROVENANCE': str(args.native_store_provenance)})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image')
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--case')
    parser.add_argument('--mode', choices=('case', 'list', 'observe'), default='case')
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--native-root', type=Path)
    parser.add_argument('--child', action='store_true')
    parser.add_argument('--pidns-fd', type=int)
    parser.add_argument('--native-store-provenance', type=Path, required=True)
    args = parser.parse_args()
    if args.archive or args.child:
        native(args)
        return
    if os.geteuid() == 0:
        raise ValueError('image admission/export must run through rootless Podman')
    name = 'tilo-native-filesystem-' + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='tilo-rootless-native-') as scratch:
        archive = Path(scratch) / 'rootfs.tar'
        created = False
        try:
            checked(['podman', 'create', '--name', name, '--network=none', args.image, '/usr/bin/true'],
                    stdout=subprocess.DEVNULL)
            created = True
            checked(['podman', 'export', '--output', archive, name], stdout=subprocess.DEVNULL)
            archive.chmod(0o600)
        finally:
            if created:
                checked(['podman', 'rm', '--force', name], stdout=subprocess.DEVNULL)
                if subprocess.run(['podman', 'container', 'exists', name]).returncode != 1:
                    raise ValueError('rootless filesystem container retirement unknown')
        # Privileged imports must not leave root-owned bytecode in the
        # operator's staged input tree, preventing its normal retirement.
        command = ['sudo', '-n', sys.executable, '-B', __file__, '--archive', archive,
                   '--stage', args.stage, '--evidence', args.evidence, '--mode', args.mode,
                   '--native-store-provenance', args.native_store_provenance]
        if args.case:
            command += ['--case', args.case]
        child = subprocess.Popen(list(map(str, command)), stdin=subprocess.PIPE)
        try:
            if child.wait():
                raise subprocess.CalledProcessError(child.returncode, child.args)
        finally:
            child.stdin.close()


if __name__ == '__main__':
    main()
