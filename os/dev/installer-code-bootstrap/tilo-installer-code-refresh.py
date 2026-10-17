#!/usr/bin/env python3
"""Refresh allowlisted installer helpers inside an explicitly attached dev VM.

Only the /etc seed on old immutable CI media runs this copy; new media run the
compiled daemon, `tilo-vm-install-proof code-refresh`.
"""
import fcntl
import importlib.util
import json
import hashlib
import os
from pathlib import Path
import socket
import stat
import subprocess
import time

BASE = Path('/run/tilo-installer/code-refresh')
ROOT = Path('/')
PORT = Path('/dev/virtio-ports/org.tilo.installer-code')
PROOF_PORT = Path('/dev/virtio-ports/org.tilo.vm-install-proof')
MARKER = Path('/usr/share/tilo/unverified-vm-install')
FLAVOUR = Path('/usr/lib/tilo/image-flavour')
MODULE = Path(__file__).with_name('tilo-installer-code-protocol.py')
spec = importlib.util.spec_from_file_location('installer_code_protocol', MODULE)
protocol = importlib.util.module_from_spec(spec)
spec.loader.exec_module(protocol)
require = protocol.require


def command(*argv):
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=30)
    except subprocess.TimeoutExpired as error:
        raise ValueError(f"{Path(argv[0]).name} timed out refreshing installer code") from error
    require(result.returncode == 0, f'{Path(argv[0]).name} failed refreshing installer code')
    return result.stdout.strip()


def gate():
    require(os.geteuid() == 0 and MARKER.is_file() and not MARKER.is_symlink()
            and FLAVOUR.is_file() and not FLAVOUR.is_symlink() and FLAVOUR.read_bytes() == b'dev\n'
            and PORT.is_char_device() and PROOF_PORT.is_char_device()
            and 'tilo.install' in Path('/proc/cmdline').read_text().split(),
            'installer code refresh requires the opted-in development scenario VM')
    command('systemd-detect-virt', '--vm', '--quiet')
    require(os.readlink('/proc/self/ns/mnt') == os.readlink('/proc/1/ns/mnt'),
            'installer code refresh must run in the PID1 mount namespace')


def lock():
    BASE.mkdir(mode=0o700, parents=True, exist_ok=True)
    require(not BASE.is_symlink() and BASE.stat().st_uid == 0
            and stat.S_IMODE(BASE.stat().st_mode) == 0o700,
            'unsafe code refresh runtime directory')
    descriptor = os.open(BASE / 'lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    require(stat.S_ISREG(os.fstat(descriptor).st_mode) and os.fstat(descriptor).st_uid == 0,
            'unsafe installer code refresh lock')
    return descriptor


def idle():
    # Independent of the replaceable helper's flock protocol: older helpers
    # must not be refreshed while an installer launcher/backend is alive.
    units = command('systemctl', 'list-units', '--all', '--plain', '--no-legend',
                    'tilo-install-*.service')
    for row in units.splitlines():
        fields = row.split()
        require(len(fields) >= 4 and fields[2] not in ('active', 'activating', 'deactivating'),
                'installer transient unit is running; code refresh refused')
    for path in Path('/proc').glob('[0-9]*/cmdline'):
        try:
            arguments = path.read_bytes().split(b'\0')
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        require(not any(Path(os.fsdecode(arg)).name == 'tilo-install' for arg in arguments if arg),
                'installer helper process is running; code refresh refused')


def apply(files, receipt):
    gate()
    descriptor = lock()
    mounted = []
    marker = BASE / 'active.json'
    previous = None
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise ValueError('installer backend is running; code refresh refused') from exc
        idle()
        if marker.is_file():
            previous = json.loads(marker.read_text())
            require(previous.get('state') == 'applied', 'previous refresh is incomplete; restart this VM')
            if {key: value for key, value in previous.items() if key != 'state'} == receipt:
                return receipt
        require(len(list(BASE.glob('[0-9a-f]' * 64))) < 32,
                '32 helper generations retained; restart the disposable installer VM')
        generation = BASE / receipt['sha256']
        require(not generation.exists() and not generation.is_symlink(),
                'this installer code generation was already attempted; restart this VM')
        generation.mkdir(mode=0o700)
        staged = []
        # Validate and prepare ALL files before changing any executable view.
        for relative, data in files.items():
            destination = ROOT / relative
            require(destination.is_file() and not destination.is_symlink(),
                    'code refresh destination is not an existing regular helper')
            original = destination.stat()
            source = generation / destination.name
            source.write_bytes(data)
            source.chmod(stat.S_IMODE(original.st_mode))
            os.chown(source, original.st_uid, original.st_gid)
            if 'security.selinux' in os.listxattr(destination):
                os.setxattr(source, 'security.selinux', os.getxattr(destination, 'security.selinux'))
            staged.append((source, destination))
        idle()
        marker.write_text(json.dumps(dict(receipt, state='applying'), sort_keys=True) + '\n')
        marker.chmod(0o600)
        try:
            for source, destination in staged:
                command('mount', '--bind', str(source), str(destination))
                mounted.append(destination)
                command('mount', '-o', 'remount,bind,ro', str(destination))
            marker.write_text(json.dumps(dict(receipt, state='applied'), sort_keys=True) + '\n')
        except (ValueError, OSError):
            try:
                for destination in reversed(mounted):
                    command('umount', str(destination))
                if previous is not None:
                    marker.write_text(json.dumps(previous, sort_keys=True) + '\n')
                else:
                    marker.unlink()
            except (ValueError, OSError):
                # Do not let an old UI/backend lacking the new lock protocol
                # operate with a partial view. This is a disposable dev VM.
                command('systemctl', 'stop', 'greetd.service')
            raise
        return receipt
    finally:
        os.close(descriptor)


def backend_probe(files):
    helper = files.get('usr/libexec/tilo-install', b'')
    if b'# TILO_VM_CODE_REFRESH_PROBE_V1:' not in helper:
        return None
    arguments = ['/usr/libexec/tilo-install', '--dev-code-refresh-probe']
    bootstrap = BASE / 'bootstrap.json'
    if Path('/dev/disk/by-id/virtio-tilo-code-fixture').exists():
        arguments.append('--cached-target')
    result = subprocess.run(arguments, input=b'',
                            capture_output=True, timeout=30)
    require(result.returncode == 0 and len(result.stdout) <= 16384,
            'refreshed backend execution probe failed (exit ' + str(result.returncode) + '): '
            + (result.stdout[-4096:] + result.stderr[-4096:]).decode('utf-8', 'replace'))
    rows = [json.loads(line) for line in result.stdout.splitlines() if line.startswith(b'{')]
    require(len(rows) == 1 and rows[0].get('schema') == 'tilo.installer-code-backend-probe.v1'
            and rows[0].get('transient') == 1 and rows[0].get('enforcing') is True
            and rows[0].get('helper_sha256') == hashlib.sha256(helper).hexdigest()
            and rows[0].get('domain', '').split(':')[2:3] in (['init_t'], ['unconfined_service_t']),
            'backend probe does not match the executed enforcing transient helper')
    return rows[0]


def ready():
    address = os.environ.get('NOTIFY_SOCKET')
    if address:
        if address.startswith('@'):
            address = '\0' + address[1:]
        with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as client:
            client.connect(address)
            client.sendall(b'READY=1')


def bootstrap_receive(stream):
    for attempt in range(64):
        hello = stream.readline(1025)
        require(0 < len(hello) <= 1024, 'bootstrap hello missing or oversized')
        if hello == b'TILO-CODE-HELLO-1\n':
            break
    else:
        raise ValueError('bootstrap hello was not recognized')
    for attempt in range(64):
        protocol.write_all(stream, b'{"schema":"tilo.installer-code-ready.v1"}\n')
        prefix = protocol.read_exact(stream, 4)
        if prefix == b'TILO':
            require(protocol.read_exact(stream, 14) == b'-CODE-HELLO-1\n', 'invalid repeated bootstrap hello')
            continue
        size = protocol.struct.unpack('!I', prefix)[0]
        require(0 < size <= protocol.LIMIT, 'code refresh packet exceeds its bound')
        return protocol.validate(protocol.read_exact(stream, size))
    raise ValueError('too many repeated bootstrap hellos')


def main():
    gate()
    ready_sent = False
    while True:
        # Host chardev connections may close between refreshes. Reopen the
        # guest port rather than spinning on EOF or treating it as new code.
        with PORT.open('r+b', buffering=0) as stream:
            try:
                if os.environ.get('TILO_CODE_BOOTSTRAP_HANDSHAKE') == '1':
                    files, receipt = bootstrap_receive(stream)
                else:
                    files, receipt = protocol.receive(stream)
                applied = apply(files, receipt)
                response = {'ok': True, 'receipt': applied, 'runtime': {
                    'schema':'tilo.installer-code-runtime-probe.v1', 'backend':backend_probe(files)}}
                if not ready_sent:
                    ready()
                    ready_sent = True
            except (ValueError, OSError, json.JSONDecodeError, subprocess.TimeoutExpired) as error:
                response = {'ok': False, 'error': str(error)}
            try:
                protocol.write_all(stream, protocol.canonical(response) + b'\n')
            except (ValueError, OSError):
                pass
        time.sleep(0.1)


if __name__ == '__main__':
    main()
