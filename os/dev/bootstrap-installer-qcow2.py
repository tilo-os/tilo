#!/usr/bin/env python3
"""Seed a NEW VM-only qcow2 overlay's bootc /etc; never change CI UKI/CFS.

Run as root in a private Linux mount namespace. Relative backing filenames
make the retained overlay usable by both Windows QEMU and WSL tools.
"""
import argparse
from contextlib import ExitStack, contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import re
import stat
import struct
import subprocess
import tempfile
import time

SCHEMA = 'tilo.installer-code-bootstrap.v1'
FILES = ('tilo-installer-code-bootstrap.py', 'tilo-installer-code-refresh.py',
         'tilo-installer-code-protocol.py', 'tilo-vm-install-proof.py')
# Where the checkout keeps each seeded file. Every one only ever runs from the
# seed on old media (new media carry the compiled refresh daemon and proof
# emitter), so they live with the host tooling.
SOURCES = {name: 'os/dev/installer-code-bootstrap/' + name for name in FILES}
UNIT = '[Unit]\nDescription=Local development installer bootstrap (VM only)\nConditionVirtualization=vm\nConditionKernelCommandLine=tilo.install\nWants=systemd-udev-settle.service\nRequires=tilo-installer-setup.service\nAfter=local-fs.target systemd-udev-settle.service tilo-installer-setup.service\nBefore=greetd.service\n[Service]\nType=notify\nNotifyAccess=main\nTimeoutStartSec=60\nUMask=0077\nExecStart=/usr/bin/python3 /etc/tilo/installer-code-bootstrap/tilo-installer-code-bootstrap.py\nPrivateMounts=no\nNoNewPrivileges=no\n[Install]\nWantedBy=multi-user.target\n'
NAME = 'tilo-installer-code-bootstrap.service'


def require(ok, message):
    if not ok:
        raise ValueError(message)


def command(*argv):
    result = subprocess.run(argv, capture_output=True, timeout=60)
    require(result.returncode == 0, Path(argv[0]).name + ' failed: ' + result.stderr[-4096:].decode('utf-8', 'replace'))
    return result.stdout


def trusted(root, relative):
    path = root
    for part in Path(relative).parts:
        require(part not in ('..', '/') and not path.is_symlink(), 'redirected bootstrap path')
        path = path / part
        require(not path.is_symlink(), 'redirected bootstrap path')
    return path


def contents(path, limit=512 * 1024):
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= limit,
            'bootstrap input is missing, redirected or oversized')
    data = path.read_bytes()
    require(data, 'bootstrap input must be nonempty')
    return data


def sealed_cmdline(data):
    """The UKI's .cmdline file offset and size (the smaller of raw and virtual)."""
    pe = struct.unpack_from('<I', data, 0x3C)[0]
    require(data[pe:pe + 4] == b'PE\0\0', 'CI UKI is not a PE image')
    count = struct.unpack_from('<H', data, pe + 6)[0]
    base = pe + 24 + struct.unpack_from('<H', data, pe + 20)[0]
    for index in range(count):
        header = base + index * 40
        if data[header:header + 8].rstrip(b'\0') == b'.cmdline':
            size, _address, raw_size, offset = struct.unpack_from('<IIII', data, header + 8)
            return offset, min(size, raw_size)
    raise ValueError('CI UKI has no .cmdline section')


def selected_deployment(esp):
    entries = trusted(esp, 'loader/entries')
    names = list(entries.glob('*.conf'))
    require(len(names) == 1, 'bootstrap requires one selected CI boot entry')
    lines = contents(names[0], 16384).decode().splitlines()
    values = [line.split(None, 1)[1] for line in lines if line.startswith('uki ')]
    require(len(values) == 1, 'CI boot entry has no unique UKI')
    match = re.fullmatch(r'/EFI/Linux/bootc/bootc_composefs-([0-9a-f]{128})[.]efi', values[0])
    require(match is not None, 'CI boot entry is not a composefs UKI')
    path = trusted(esp, values[0].lstrip('/'))
    require(path.is_file() and path.stat().st_size <= 256 * 1024 * 1024, 'invalid CI UKI')
    data = path.read_bytes()
    offset, length = sealed_cmdline(data)
    tokens = data[offset:offset + length].rstrip(b'\0').decode('utf-8').split()
    require('tilo.install' in tokens and 'composefs=' + match[1] in tokens,
            'bootstrap requires the exact installer UKI deployment')
    return match[1], hashlib.sha256(data).hexdigest()


def extract_boot(esp_device, destination):
    # Standard WSL kernels omit VFAT. mtools reads just the selected entry and
    # UKI without mounting or writing the ESP; no full-disk copy is performed.
    entries = destination / 'loader/entries'; entries.mkdir(parents=True)
    command('mcopy', '-i', str(esp_device), '::/loader/entries/*.conf', str(entries))
    names = list(entries.glob('*.conf'))
    require(len(names) == 1, 'CI ESP has no unique selected entry')
    values = [line.split(None, 1)[1] for line in contents(names[0], 16384).decode().splitlines()
              if line.startswith('uki ')]
    require(len(values) == 1 and re.fullmatch(r'/EFI/Linux/bootc/bootc_composefs-[0-9a-f]{128}[.]efi', values[0]),
            'CI ESP entry is not a selected composefs UKI')
    target = destination / values[0].lstrip('/'); target.parent.mkdir(parents=True)
    command('mcopy', '-i', str(esp_device), '::' + values[0], str(target))


def seed(root, esp, checkout, receipt):
    digest, uki_hash = selected_deployment(esp)
    etc = trusted(root, 'state/deploy/' + digest + '/etc')
    require(etc.is_dir() and trusted(root, 'composefs').is_dir(), 'selected bootc mutable /etc is missing')
    unit_dir = trusted(etc, 'systemd/system')
    existing = unit_dir / 'multi-user.target.wants/tilo-installer-code-refresh.service'
    require(not existing.exists() and not existing.is_symlink(),
            'CI medium already has the refresh daemon; use its normal local code channel')
    unit_label = os.getxattr(unit_dir, 'security.selinux')
    etc_label = os.getxattr(etc, 'security.selinux')
    require(b':systemd_unit_file_t:' in unit_label and b':etc_t:' in etc_label,
            'original CI unit and /etc labels differ')
    bin_label = unit_label.replace(b':systemd_unit_file_t:', b':bin_t:')
    data = {name: contents(checkout / SOURCES[name]) for name in FILES}
    require(all(b'\r' not in value for value in data.values()), 'bootstrap scripts require LF line endings')
    destination = trusted(etc, 'tilo/installer-code-bootstrap')
    require(not destination.exists(), 'bootstrap seed already exists')
    for relative in ('tilo', 'tilo/installer-code-bootstrap', 'systemd/system/greetd.service.d',
                     'systemd/system/multi-user.target.wants'):
        directory = trusted(etc, relative)
        if not directory.exists():
            directory.mkdir(mode=0o755)
            os.setxattr(directory, 'security.selinux', unit_label if relative.startswith('systemd') else etc_label)
        require(directory.is_dir(), 'bootstrap parent is not a directory')
    observed_layout = {'composefs_images': sorted(str(path.relative_to(root)) for path in trusted(root, 'composefs/images').iterdir()),
                       'deployment_files': sorted(str(path.relative_to(root)) for path in trusted(root, 'state/deploy/' + digest).iterdir()),
                       'deploy_origin_siblings': sorted(str(path.relative_to(root)) for path in trusted(root, 'state/deploy').glob('*.origin'))}
    if receipt.get('cached_source'):
        observations = []
        for prefix in ('var', 'state/var', 'state/deploy/' + digest + '/var'):
            store = root / prefix / 'lib/containers/storage'
            layers_path, images_path = store/'overlay-layers/layers.json', store/'overlay-images/images.json'
            if not layers_path.is_file() or not images_path.is_file(): continue
            layers = json.loads(contents(layers_path, 16*1024*1024))
            images = json.loads(contents(images_path, 8*1024*1024))
            identifiers = {row['id'] for row in layers}
            observations.append({'store':str(store.relative_to(root)), 'layer_count':len(layers),
                'missing_parents':[{'id':row['id'],'parent':row['parent']} for row in layers if row.get('parent') and row['parent'] not in identifiers],
                'images':[{'id':row['id'],'layer':row['layer']} for row in images]})
        observed_layout['cache_metadata'] = observations
    manifest = dict(receipt, composefs=digest, uki_sha256=uki_hash, root_layout=observed_layout,
                    files={name: hashlib.sha256(value).hexdigest() for name, value in data.items()},
                    unit_sha256=hashlib.sha256(UNIT.encode()).hexdigest(),
                    labels={'unit':unit_label.decode(), 'etc':etc_label.decode(), 'script':bin_label.decode()})
    for name, value in data.items():
        path = destination / name
        with path.open('xb') as stream: stream.write(value); stream.flush(); os.fsync(stream.fileno())
        path.chmod(0o755); os.setxattr(path, 'security.selinux', bin_label)
    record = destination / 'receipt.json'
    record.write_text(json.dumps(manifest, sort_keys=True) + '\n'); record.chmod(0o600)
    os.setxattr(record, 'security.selinux', etc_label)
    unit = trusted(unit_dir, NAME)
    require(not unit.exists(), 'bootstrap unit already exists')
    unit.write_text(UNIT); unit.chmod(0o644); os.setxattr(unit, 'security.selinux', unit_label)
    dropin = trusted(unit_dir, 'greetd.service.d/20-installer-code-bootstrap.conf')
    require(not dropin.exists(), 'bootstrap greeter ordering already exists')
    dropin.write_text('[Unit]\nRequires=' + NAME + '\nAfter=' + NAME + '\n')
    dropin.chmod(0o644); os.setxattr(dropin, 'security.selinux', unit_label)
    link = trusted(unit_dir, 'multi-user.target.wants/' + NAME)
    require(not link.exists(), 'bootstrap enable link already exists')
    link.symlink_to('/etc/systemd/system/' + NAME)
    return manifest


@contextmanager
def reserve_nbd():
    for path in sorted(Path('/sys/class/block').glob('nbd[0-9]*')):
        if not re.fullmatch('nbd[0-9]+', path.name): continue
        lock = os.open('/run/tilo-bootstrap-' + path.name + '.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError: continue
            if (path / 'pid').exists() or (path / 'size').read_text().strip() != '0': continue
            device = Path('/dev') / path.name
            require(device.is_block_device(), 'NBD device node is missing')
            yield device
            return
        finally:
            os.close(lock)
    raise ValueError('no free NBD device; bootstrap did not attach any disk')


def owned_nbd_process(pid, device, overlay):
    try:
        arguments = Path('/proc/' + pid + '/cmdline').read_bytes().split(b'\0')
    except FileNotFoundError:
        return False
    filename = b'file.filename=' + os.fsencode(str(overlay)).replace(b',', b',,')
    attached = os.fsencode(str(overlay)) in arguments or any(
        filename + b',' in argument or argument.endswith(filename) for argument in arguments)
    return os.fsencode(str(device)) in arguments and attached \
        and Path(os.fsdecode(arguments[0])).name == 'qemu-nbd'


def disconnect_owned(pid, device, overlay):
    # WSL's sysfs NBD PID is in the outer kernel namespace, not necessarily
    # /proc's PID namespace. Bind cleanup to the recorded daemon's exact argv.
    require(owned_nbd_process(pid, device, overlay), 'owned NBD daemon disappeared; refusing ambiguous disconnect')
    command('qemu-nbd', '--disconnect', str(device))


def git_source(checkout):
    environment = dict(os.environ)
    environment.pop('GIT_DIR', None); environment.pop('GIT_WORK_TREE', None)
    gitfile = checkout / '.git'
    if gitfile.is_file():
        line = gitfile.read_text().strip()
        require(line.startswith('gitdir: '), 'invalid owner Git worktree pointer')
        directory = line[8:]
        if re.match(r'^[A-Za-z]:[/\\]', directory):
            directory = command('wslpath', '-u', directory).decode().strip()
        environment.update(GIT_DIR=str((checkout / directory).resolve()), GIT_WORK_TREE=str(checkout))
    def git(*args):
        return subprocess.run(['git', '-C', str(checkout), *args], env=environment,
                              check=True, capture_output=True, timeout=30).stdout
    revision = git('rev-parse', 'HEAD').decode().strip()
    require(re.fullmatch('[0-9a-f]{40}', revision), 'invalid owner source revision')
    dirty = bool(git('status', '--porcelain', '--', *SOURCES.values()))
    return revision, dirty


def create(source, output, checkout, verified, cached_source=None, host_qemu_img=None):
    started = time.monotonic()
    require(os.geteuid() == 0 and os.readlink('/proc/self/ns/mnt') != os.readlink('/proc/1/ns/mnt')
            and not any('shared:' in line.split(' - ')[0] for line in Path('/proc/self/mountinfo').read_text().splitlines()),
            'bootstrap requires root and a private mount namespace')
    source = source.resolve(strict=True); checkout = checkout.resolve(strict=True)
    require(source.is_file() and not output.exists() and not output.is_symlink()
            and output.parent.is_dir() and source != output.resolve(), 'CI source must be regular and overlay must be new')
    before = source.stat()
    authority = json.loads(contents(verified, 65536))
    require(authority.get('schema') == 'tilo.ci-dev-installer-download.v1'
            and authority.get('bytes') == before.st_size
            and PureWindowsPath(authority.get('disk','')).name == source.name
            and re.fullmatch('[0-9a-f]{64}', authority.get('sha256',''))
            and authority['authority'].get(source.name + '.sha256') == authority['sha256'],
            'CI download receipt does not bind this base')
    info = json.loads(command('qemu-img', 'info', '--output=json', '-f', 'qcow2', str(source)))
    require(info.get('format') == 'qcow2' and 'backing-filename' not in info, 'CI base must be standalone qcow2')
    revision, dirty = git_source(checkout)
    receipt = {'schema':SCHEMA, 'source':{'revision':revision, 'dirty':dirty},
               'base_sha256':authority['sha256'], 'base_bytes':before.st_size,
               'base_authority':'existing-ci-download-receipt', 'physical_admission':False}
    backing_source = source
    cached_before = None
    if cached_source is not None:
        cached_source = cached_source.resolve(strict=True)
        require(cached_source.is_file() and cached_source.name == 'installer-work.qcow2'
                and re.fullmatch(r'scenario-install-[0-9TZ-]+-[0-9]+', cached_source.parent.name),
                'cache must be an explicitly retained scenario installer-work')
        if host_qemu_img:
            chain = json.loads(command(str(host_qemu_img), 'info', '--backing-chain', '--output=json',
                                       command('wslpath', '-w', str(cached_source)).decode().strip()))
            for row in chain:
                row['filename'] = command('wslpath', '-u', row['filename'].removeprefix('\\\\?\\')).decode().strip()
        else:
            chain = json.loads(command('qemu-img', 'info', '--backing-chain', '--output=json', str(cached_source)))
        require(2 <= len(chain) <= 4 and all(row['format'] == 'qcow2' for row in chain)
                and Path(chain[-1]['filename']).resolve() == source
                and chain[0]['virtual-size'] >= info['virtual-size']
                and chain[0]['virtual-size'] - info['virtual-size'] <= 64*1024*1024
                and all(row['virtual-size'] == chain[0]['virtual-size'] for row in chain[:-1]),
                'retained cache chain does not end at this exact CI base')
        cached_before = cached_source.stat()
        receipt['cached_source'] = {'kind':'unverified-retained-scenario-work',
            'path':str(cached_source), 'bytes':cached_before.st_size,
            'mtime_ns':cached_before.st_mtime_ns,
            'chain':[{'filename':row['filename'], 'format':row['format'],
                      'virtual_size':row['virtual-size']} for row in chain]}
        backing_source = cached_source
    backing = os.path.relpath(backing_source, output.parent)
    virtual_size = chain[0]['virtual-size'] if cached_source else info['virtual-size']
    command('qemu-img', 'create', '-u', '-f', 'qcow2', '-F', 'qcow2', '-b', backing, str(output), str(virtual_size))
    image_options = 'driver=qcow2,file.driver=file,file.filename=' + str(output).replace(',', ',,')
    if cached_source:
        for depth, row in enumerate(chain, 1):
            prefix = 'backing.' * depth
            image_options += ',' + prefix + 'driver=qcow2,' + prefix + 'file.driver=file,' + prefix + 'file.filename=' + row['filename'].replace(',', ',,')
    else:
        image_options += ',backing.driver=qcow2,backing.file.driver=file,backing.file.filename=' + str(source).replace(',', ',,')
    with tempfile.TemporaryDirectory(prefix='tilo-code-bootstrap-') as temporary, ExitStack() as cleanup:
        scratch = Path(temporary)
        device = cleanup.enter_context(reserve_nbd())
        pidfile = scratch / 'nbd.pid'
        command('qemu-nbd', '--fork', '--pid-file', str(pidfile), '--connect', str(device), '--image-opts', image_options)
        owned_pid = pidfile.read_text().strip()
        require(owned_nbd_process(owned_pid, device, output), 'attached NBD daemon identity differs')
        cleanup.callback(disconnect_owned, owned_pid, device, output)
        deadline = time.monotonic() + 10
        sysfs = Path('/sys/class/block') / device.name
        while not ((sysfs / 'pid').exists()
                   and (sysfs / 'size').read_text().strip() == str(virtual_size // 512)
                   and all(Path(str(device) + 'p' + str(number)).is_block_device() for number in (1,2))):
            require(owned_nbd_process(owned_pid, device, output), 'owned NBD attach process failed')
            require(time.monotonic() < deadline, 'owned NBD partitions did not settle')
            time.sleep(0.05)
        layout = json.loads(command('sfdisk', '--json', str(device)))['partitiontable']
        require(layout.get('label') == 'gpt' and layout.get('device') == str(device)
                and layout.get('sectorsize') == 512 and len(layout.get('partitions', [])) == 2, 'unexpected CI installer GPT')
        parts = layout['partitions']
        require(parts[0].get('type', '').lower() == 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'
                and parts[1].get('type', '').lower() == '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab',
                'unexpected CI installer partition types: ' + repr([(row.get('name'), row.get('type')) for row in parts]))
        require([row.get('node') for row in parts] == [str(device) + 'p1', str(device) + 'p2']
                and [row.get('name') for row in parts] == ['esp', 'Tilo root'], 'CI partition identities differ')
        esp, root = scratch / 'esp', scratch / 'root'; esp.mkdir(); root.mkdir()
        extract_boot(Path(str(device) + 'p1'), esp)
        kind = command('blkid', '-s', 'TYPE', '-o', 'value', str(device) + 'p2').decode().strip()
        require(kind in ('btrfs', 'ext4'), 'unexpected CI root filesystem')
        command('mount', '-t', kind, '-o', 'rw,nosuid,nodev,noexec', str(device) + 'p2', str(root))
        cleanup.callback(command, 'umount', str(root))
        manifest = seed(root, esp, checkout, receipt)
        command('sync', '-f', str(root))
    after = source.stat()
    require((before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
            (after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'CI base changed during bootstrap')
    if cached_before is not None:
        cached_after = cached_source.stat()
        require((cached_before.st_ino, cached_before.st_size, cached_before.st_mtime_ns, cached_before.st_ctime_ns)
                == (cached_after.st_ino, cached_after.st_size, cached_after.st_mtime_ns, cached_after.st_ctime_ns),
                'retained cache changed during bootstrap')
    inventory = json.loads(command('qemu-img', 'map', '--output=json', '--image-opts', image_options))
    allocated = sum(row['length'] for row in inventory if row.get('depth') == 0 and (row.get('data') or row.get('present')))
    # A stopped cache can contain an unreplayed ext4 journal. Replaying it
    # writes ONLY the new overlay; account for bounded metadata replay too.
    budget = (128 if cached_source else 64) * 1024 * 1024
    require(allocated <= budget, 'bootstrap overlay exceeds its bounded changed guest extents')
    require(output.stat().st_size <= budget, 'bootstrap qcow2 file exceeds its bounded size')
    overlay_hash = hashlib.sha256(output.read_bytes()).hexdigest()
    manifest.update(overlay=output.name, overlay_sha256=overlay_hash, backing=backing, changed_extent_bytes=allocated, bootstrap_seconds=time.monotonic() - started)
    record = Path(str(output) + '.bootstrap.json')
    require(not record.exists(), 'bootstrap host receipt already exists')
    record.write_text(json.dumps(manifest, indent=2) + '\n')
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'output', 'checkout', 'verified'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--cached-source', type=Path, help='explicit stopped scenario installer-work; always unverified VM-only')
    parser.add_argument('--host-qemu-img', type=Path, help='Windows qemu-img.exe for Windows absolute backing-chain metadata')
    args = parser.parse_args()
    try:
        print(json.dumps(create(args.source, args.output, args.checkout, args.verified, args.cached_source, args.host_qemu_img)))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f'VM bootstrap: {error}\n')


if __name__ == '__main__':
    main()
