#!/usr/bin/env python3
"""Run a diagnostic command on private virtio Btrfs stores in an owned dev VM.

Only a SHA256-verified published disk in tilo-vm's cache is accepted as the
base. The base is not candidate-source evidence. All guest execution uses
`tilo-vm vm ssh`; no daemon, privileged transport or desktop control is added.
A successful diagnostic is not current-source qualification.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shlex
import signal
import subprocess
import tempfile
import xml.etree.ElementTree as ET


def run(args, **kwargs):
    try:
        return subprocess.run([str(a) for a in args], check=True, text=True, **kwargs)
    except subprocess.CalledProcessError as error:
        if error.stderr:
            print(error.stderr, end='', flush=True)
        raise


def guest(cli, vm, command, **kwargs):
    return run([cli, 'vm', 'ssh', vm, '--', command], **kwargs)


def verify_base(base):
    cache = Path.home() / '.cache/tilo-vm/disks'
    base = base.resolve(strict=True)
    if not base.is_relative_to(cache.resolve()) or base.suffix != '.qcow2':
        raise ValueError('base must be a published qcow2 in the tilo-vm disk cache')
    expected = base.with_suffix('.qcow2.sha256').read_text().split()[0]
    with base.open('rb') as stream:
        actual = hashlib.file_digest(stream, 'sha256').hexdigest()
    if actual != expected:
        raise ValueError('cached published disk checksum differs')
    return base, actual


def attach(vm, directory, rows):
    for row in rows:
        disk = directory / (row['name'] + '.raw')
        fd = os.open(disk, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.truncate(row['size'])
        tree = ET.Element('disk', type='file', device='disk')
        ET.SubElement(tree, 'driver', name='qemu', type='raw')
        ET.SubElement(tree, 'source', file=str(disk))
        ET.SubElement(tree, 'target', dev='vd' + ('b' if row['name'] == 'a' else 'c'), bus='virtio')
        ET.SubElement(tree, 'serial').text = row['serial']
        xml = directory / (row['name'] + '.xml')
        ET.ElementTree(tree).write(xml)
        run(['virsh', '-c', 'qemu:///session', 'attach-device', vm, xml, '--config'])


def exercise(cli, vm, directory, receipt, command):
    helper = Path(__file__).with_name('native-store-virtio.py').read_text()
    shell = Path(__file__).with_name('native-stores.sh').read_text()
    # stdin over the existing transport, into a newly created root-owned guest
    # directory. No installed role or source manifest is modified/restamped.
    bootstrap = (
        'import os,pathlib,sys; '
        f'p=pathlib.Path({str(directory)!r}); p.mkdir(mode=0o700); '
        f'(p/"provenance.json").write_text({json.dumps(receipt)!r}); '
        'os.chmod(p/"provenance.json",0o600); '
        '(p/"native-store-virtio.py").write_text(sys.stdin.read()); '
        f'(p/"native-stores.sh").write_text({shell!r})'
    )
    guest(cli, vm, shlex.join(['python3', '-c', bootstrap]), input=helper)
    setup = shlex.join(['env', f'TILO_NATIVE_STORE_VIRTIO_PROVENANCE={directory}/provenance.json',
                        'bash', str(directory / 'native-stores.sh'), 'up', str(directory)])
    teardown = setup.replace(' up ', ' down ')
    try:
        env = guest(cli, vm, setup, capture_output=True).stdout
        assignments = [line for line in env.splitlines() if line.startswith('TILO_')]
        expected = {'TILO_FS_NATIVE_STORES', 'TILO_FS_NATIVE_STORE', 'TILO_REQUIRE_FANOTIFY'}
        if {line.split('=', 1)[0] for line in assignments} != expected:
            raise ValueError('native store setup did not return the required environment')
        print(env, end='', flush=True)
        guest(cli, vm, shlex.join(['env', *assignments, *command]))
    finally:
        guest(cli, vm, teardown)


def cleanup(cli, vm, directory):
    registry = directory / 'vms.toml'
    # A private registry avoids changing the operator's default or racing a
    # shared registry write. If create registered, use its normal removal path.
    if registry.exists():
        run([cli, 'vm', 'rm', vm, '--force'])
    else:
        # vm create can fail after defining libvirt but before registry.save.
        # This name and all paths were generated in our private directory.
        inventory = run(['virsh', '-c', 'qemu:///session', 'list', '--all', '--name'],
                        capture_output=True)
        if vm in inventory.stdout.splitlines():
            domain = run(['virsh', '-c', 'qemu:///session', 'dumpxml', vm], capture_output=True)
            tree = ET.fromstring(domain.stdout)
            sources = [node.get('file') for node in tree.findall('./devices/disk/source')]
            if str(directory / 'root.qcow2') not in sources:
                raise ValueError('refusing cleanup of a domain not backed by our owned child')
            state = run(['virsh', '-c', 'qemu:///session', 'domstate', vm], capture_output=True)
            if state.stdout.strip() != 'shut off':
                raise ValueError('partially created domain unexpectedly running; backing retained')
            run(['virsh', '-c', 'qemu:///session', 'undefine', vm, '--nvram'])
    # Keep bytes on any domain-removal failure, never unlink a running disk.
    import shutil
    shutil.rmtree(directory)


def cancelled(signum, _frame):
    raise RuntimeError(f'disposable VM interrupted by signal {signum}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tilo-vm', type=Path, default=Path('target/debug/tilo-vm'))
    parser.add_argument('--base', type=Path, required=True)
    parser.add_argument('command', nargs=argparse.REMAINDER,
                        help='diagnostic guest command after -- (not qualification evidence)')
    args = parser.parse_args()
    command = args.command
    if command[:1] == ['--']:
        command = command[1:]
    if not command:
        parser.error('a guest diagnostic command is required')
    base, digest = verify_base(args.base)
    token = secrets.token_hex(8)
    vm = 'native-store-private-' + token
    rows = [{'name': name, 'serial': f'tns-{token[:12]}-{name}', 'size': 1024 ** 3} for name in ('a', 'b')]
    receipt = {'schema': 1, 'vm': vm, 'base_sha256': digest, 'disks': rows}
    print(json.dumps(receipt), flush=True)
    # Keep files until domain removal succeeds: never unlink a running disk.
    parent = Path.home() / '.local/share/tilo-vm/vms'
    parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix=vm + '-', dir=parent))
    os.environ['TILO_VM_CONFIG'] = str(directory / 'vms.toml')
    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, cancelled)
    try:
        run([args.tilo_vm, 'vm', 'create', vm, '--hypervisor', 'libvirt',
             '--image', base, '--disk', directory / 'root.qcow2',
             '--memory-mb', '3072', '--cpus', '2'])
        attach(vm, directory, rows)
        # An older published disk may boot successfully but fail the daemon
        # handshake. vm ssh remains the existing independent diagnostic route.
        start = subprocess.run([str(args.tilo_vm), 'vm', 'start', vm, '--json'],
                               text=True, capture_output=True)
        if start.returncode:
            result = json.loads(start.stdout)
            if 'speaks protocol' not in result.get('error', ''):
                raise RuntimeError(start.stdout + start.stderr)
            print(result['error'], flush=True)
        exercise(args.tilo_vm, vm, Path('/var/tmp') / vm, receipt, command)
    finally:
        cleanup(args.tilo_vm, vm, directory)


if __name__ == '__main__':
    main()
