#!/usr/bin/env python3
"""Read-only installed boot/root verification in a private mount namespace.

Select installed-target or installer-media explicitly. Normal targets bind the
predicted target UKI; media bind one named, authenticated source live UKI and
retain separate target PCR proof. Neither mode proves actual boot behavior.
This is one acceptance stage, not a complete source-layer audit or VM boot test.
Only reopen the exact regular file recorded by the disk creator. Never select
an existing device. Run with: unshare --mount --propagation private -- PROGRAM.
"""
import argparse
import configparser
from contextlib import ExitStack, contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time

import development_prediction as prediction
import hardware_profile as hardware
import installed_material_audit as material
import installer_bootc_helper
from private_disk_image import _directory, loop_partition_nodes

spec = importlib.util.spec_from_file_location('installed_source', Path(__file__).with_name('verify-development-source.py'))
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)


def command(argv, *, pass_fds=(), timeout=120, output_limit=4*1024*1024):
    """Bound output in memory and kill the entire operation on timeout/failure."""
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        pass_fds=pass_fds, start_new_session=True,
        env={'PATH': '/usr/bin:/bin', 'HOME': '/root', 'LANG': 'C.UTF-8'})
    deadline = time.monotonic() + timeout
    output = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ValueError(f'{Path(argv[0]).name} exceeded operation deadline')
                for key, _ in selector.select(min(remaining, 1)):
                    chunk = os.read(key.fileobj.fileno(), 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    if len(output) + len(chunk) > output_limit:
                        raise ValueError(f'{Path(argv[0]).name} exceeded output limit')
                    output.extend(chunk)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ValueError(f'{Path(argv[0]).name} exceeded operation deadline')
        try:
            status = process.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            raise ValueError(f'{Path(argv[0]).name} exceeded operation deadline') from None
        if status:
            raise ValueError(f'{Path(argv[0]).name} failed (exit {status})')
        return bytes(output)
    finally:
        # Also terminate descendants holding stdout after their leader exited.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        process.stdout.close()


def read(root, relative, limit=1024*1024):
    path = hardware.trusted_path(root, Path(relative), regular=True)
    with path.open('rb') as f:
        value = f.read(limit + 1)
    if len(value) > limit:
        raise ValueError('installed metadata exceeds limit')
    return value


def file_identity(info):
    return {'device': info.st_dev, 'inode': info.st_ino, 'size': info.st_size}


def copied_disk_receipt(path, creation_bytes, creation, copied):
    required={'schema','purpose','source_creation_sha256','source_path',
              'source_file_identity','raw_identity','path','file_identity'}
    raw=copied.get('raw_identity') if isinstance(copied,dict) else None
    if (not isinstance(copied,dict) or set(copied)!=required
            or copied.get('schema')!=1 or copied.get('purpose')!='tilo-supervised-raw-media-copy'
            or copied.get('source_creation_sha256')!=hashlib.sha256(creation_bytes).hexdigest()
            or copied.get('source_path')!=creation.get('path')
            or copied.get('source_file_identity')!=creation.get('file_identity')
            or copied.get('path')!=str(path) or not isinstance(raw,dict)
            or set(raw)!={'size','sha256'} or raw.get('size')!=creation.get('size_bytes')
            or not re.fullmatch(r'[0-9a-f]{64}',raw.get('sha256',''))
            or not isinstance(copied.get('file_identity'),dict)
            or set(copied['file_identity'])!={'device','inode','size'}
            or copied['file_identity'].get('size')!=creation.get('size_bytes')):
        raise ValueError('copied disk receipt does not bind the created raw media')
    return {'path':str(path),'file_identity':copied['file_identity'],
            'size_bytes':creation['size_bytes'],'disk_created':True}


@contextmanager
def open_created_disk(path, receipt):
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts or str(path) != receipt.get('path'):
        raise ValueError('disk path differs from creation receipt')
    expected = receipt.get('file_identity')
    if (not isinstance(expected, dict) or set(expected) != {'device', 'inode', 'size'}
            or any(type(v) is not int or v <= 0 for v in expected.values())
            or expected['size'] != receipt.get('size_bytes') or receipt.get('disk_created') is not True):
        raise ValueError('creation receipt lacks exact file identity')
    parent = _directory(path.parent)
    try:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        try:
            def validate():
                for info in (os.fstat(fd), os.stat(path.name, dir_fd=parent, follow_symlinks=False)):
                    if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077
                            or info.st_nlink != 1 or file_identity(info) != expected):
                        raise ValueError('disk identity, type, ownership, links or mode changed')
            validate()
            before = os.fstat(fd)
            yield fd
            validate()
            after = os.fstat(fd)
            if (before.st_mtime_ns, before.st_ctime_ns) != (after.st_mtime_ns, after.st_ctime_ns):
                raise ValueError('disk was modified during verification')
        finally:
            os.close(fd)
    finally:
        os.close(parent)


def partitions(document, loop):
    table = document['partitiontable']
    if table.get('label') != 'gpt' or table.get('device') != loop or table.get('sectorsize') != 512:
        raise ValueError('expected a 512-byte-sector GPT on the allocated loop')
    rows = table['partitions']
    expected = [('EFI-SYSTEM', 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'),
                ('root', '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab')]
    if len(rows) != 2:
        raise ValueError('expected only ESP and ARM64 root partitions')
    for index, (row, (label, kind)) in enumerate(zip(rows, expected), 1):
        if row.get('node') != f'{loop}p{index}' or row.get('name') != label or row.get('type', '').lower() != kind:
            raise ValueError('partition identity or type differs')
    if rows[0]['size'] != 1024 * 1024 * 1024 // 512 or rows[0]['start'] < 2048:
        raise ValueError('unexpected ESP size/alignment')
    if rows[1]['start'] < rows[0]['start'] + rows[0]['size'] or rows[1]['size'] <= 0:
        raise ValueError('overlapping or empty root partition')
    return [row['node'] for row in rows]


def media_partitions(document, loop):
    """Validate the exact systemd-repart layout emitted by disk-image.sh."""
    table = document['partitiontable']
    if table.get('label') != 'gpt' or table.get('device') != loop or table.get('sectorsize') != 512:
        raise ValueError('expected a 512-byte-sector GPT on the allocated loop')
    rows = table['partitions']
    expected = [('esp', 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b'),
                ('Tilo root', '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab')]
    if len(rows) != 2:
        raise ValueError('expected only ESP and ARM64 root partitions')
    for index, (row, (label, kind)) in enumerate(zip(rows, expected), 1):
        if (row.get('node') != f'{loop}p{index}' or row.get('name') != label
                or row.get('type', '').lower() != kind):
            raise ValueError('media partition identity or type differs')
    if rows[0]['size'] != 2 * 1024 * 1024 * 1024 // 512 or rows[0]['start'] < 2048:
        raise ValueError('unexpected media ESP size/alignment')
    if rows[1]['start'] < rows[0]['start'] + rows[0]['size'] or rows[1]['size'] <= 0:
        raise ValueError('overlapping or empty media root partition')
    return [row['node'] for row in rows]


def config_lines(raw):
    result = {}
    for line in raw.decode('utf-8', 'strict').splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        fields = line.split(None, 1)
        if len(fields) != 2 or fields[0] in result:
            raise ValueError('ambiguous boot entry')
        result[fields[0]] = fields[1]
    return result


def selected_uki(esp, digest, *, target_materialized=False):
    entries = hardware.trusted_path(esp, Path('loader/entries'), regular=False)
    names = list(entries.iterdir())
    if len(names) != 1 or not names[0].name.endswith('.conf'):
        raise ValueError('fresh disk must have exactly one boot entry')
    entry = config_lines(read(esp, names[0].relative_to(esp)))
    wanted = f'/EFI/Linux/bootc/bootc_composefs-{digest}.efi'
    if entry.get('efi') != wanted or set(entry) - {'title', 'version', 'sort-key', 'efi'}:
        raise ValueError('selected entry differs or overrides signed UKI')
    loader = config_lines(read(esp, 'loader/loader.conf'))
    default = loader.get('default', names[0].name)
    # systemd259 install_loader_config writes `default ENTRY_TOKEN-*`.
    # bootc installs bootctl first and preserves this existing loader.conf.
    # A nonmatching token falls back to the sole suitable entry. Never accept
    # saved/oneshot selectors; firmware variable selection needs the VM test.
    if (set(loader) - {'timeout', 'default'} or
            (default != names[0].name and not re.fullmatch(r'[A-Za-z0-9_.-]+-\*', default))):
        raise ValueError('unsupported loader selection override')
    # No extra automatically discovered UKIs, drivers or UKI addons.
    for relative in ('loader/addons', 'EFI/Linux', 'EFI/systemd/drivers'):
        directory = esp / relative
        if directory.exists():
            hardware.trusted_path(esp, Path(relative), regular=False)
            files = [p.relative_to(esp).as_posix() for p in directory.rglob('*') if not p.is_dir()]
            if files != [wanted.lstrip('/')] and files:
                raise ValueError('unexpected automatically discovered EFI payload')
    allowed_efi = {wanted.lstrip('/').lower(), 'efi/systemd/systemd-bootaa64.efi', 'efi/boot/bootaa64.efi'}
    if target_materialized:
        allowed_efi.add('tilo-target/tilo-target.efi')
    for path in esp.rglob('*'):
        if path.suffix.lower() == '.efi' and path.relative_to(esp).as_posix().lower() not in allowed_efi:
            raise ValueError('unexpected alternate EFI application')
    return wanted.lstrip('/')


def verify_materialized_target(source_root, esp, prediction_receipt, generation):
    """Authenticate each exact sidecar; no additional EFI/path allowance."""
    expected = {'tilo-target.efi': prediction.boot.UKI_LIMIT,
                f'predictions/generation-{generation}.json': 16384,
                f'predictions/generation-{generation}.json.uki.sig': 1024}
    base = hardware.trusted_path(esp, Path('tilo-target'), regular=False)
    observed = set()
    for directory, dirs, files in os.walk(base, followlinks=False):
        relative = Path(directory).relative_to(esp)
        for name in dirs:
            hardware.trusted_path(esp, relative / name, regular=False)
        for name in files:
            observed.add((Path(directory) / name).relative_to(base).as_posix())
    if observed != set(expected):
        raise ValueError('materialized target inventory differs')
    rows = []
    for relative, limit in expected.items():
        original = read(source_root, 'boot/tilo-target/' + relative, limit)
        installed = read(esp, 'tilo-target/' + relative, limit)
        if installed != original:
            raise ValueError('materialized target differs from authenticated source')
        digest = hashlib.sha256(installed).hexdigest()
        if relative == 'tilo-target.efi' and digest != prediction_receipt['target_uki_sha256']:
            raise ValueError('materialized target differs from predicted UKI')
        rows.append({'path': 'tilo-target/' + relative, 'sha256': digest})
    return {'files': rows, 'source_target_prediction_verified': True,
            'runtime_access_verified': False}


def origin_identity(raw, target, manifest, sections):
    ini = configparser.ConfigParser(interpolation=None, strict=True)
    ini.read_string(raw.decode('utf-8', 'strict'))
    if (ini.get('origin', 'container-image-reference') != 'ostree-unverified-image:docker://' + target
            or ini.get('image', 'manifest_digest') != manifest or ini.get('boot', 'boot_type') != 'uki'
            or ini.get('boot', 'digest') != hashlib.sha256(sections['.linux'] + sections['.initrd']).hexdigest()):
        raise ValueError('installed origin differs from authenticated source and UKI')


def readonly_mount(path, expected_type=None):
    doc = json.loads(command(['/usr/bin/findmnt', '--json', '--mountpoint', str(path), '-o', 'TARGET,FSTYPE,OPTIONS']))
    rows = doc['filesystems']
    if len(rows) != 1 or rows[0]['target'] != str(path) or 'ro' not in rows[0]['options'].split(','):
        raise ValueError('verification mount is not independently read-only')
    if expected_type and rows[0]['fstype'] != expected_type:
        raise ValueError('installed filesystem type differs')
    return rows[0]


def preflight_disk(path):
    """Cheap read-only structural check before spending time on a VM boot."""
    if os.getuid() != 0 or os.geteuid() != 0:
        raise ValueError('disk preflight requires root')
    require_private_namespace()
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('disk preflight requires an absolute path')
    parent = _directory(path.parent)
    try:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        try:
            before = os.fstat(fd)
            if not stat.S_ISREG(before.st_mode) or before.st_size < 2 * 1024**3:
                raise ValueError('disk must be a regular image large enough for ESP and root')
            with tempfile.TemporaryDirectory(prefix='tilo-disk-preflight-') as directory, ExitStack() as cleanup:
                scratch = Path(directory)
                loop = command(['/usr/sbin/losetup', '--find', '--show', '--read-only', '--partscan',
                                f'/proc/self/fd/{fd}'], pass_fds=(fd,)).decode().strip()
                if not re.fullmatch('/dev/loop[0-9]+', loop):
                    raise ValueError('unexpected allocated loop identity')
                cleanup.callback(command, ['/usr/sbin/losetup', '--detach', loop])
                cleanup.enter_context(loop_partition_nodes(loop))
                if command(['/usr/sbin/blockdev', '--getro', loop]).strip() != b'1':
                    raise ValueError('loop is not read-only')
                layout = json.loads(command(['/usr/sbin/sfdisk', '--json', loop]))
                parts = media_partitions(layout, loop)
                esp, root = scratch / 'esp', scratch / 'root'
                for device, destination, kind, options in zip(
                        parts, (esp, root), ('vfat', 'btrfs'),
                        ('ro,nodev,nosuid,noexec', 'ro,nodev,nosuid,noexec,nologreplay')):
                    destination.mkdir()
                    command(['/usr/bin/mount', '-t', kind, '-o', options, device, str(destination)])
                    cleanup.callback(command, ['/usr/bin/umount', str(destination)])
                    readonly_mount(destination, kind)

                entries = hardware.trusted_path(esp, Path('loader/entries'), regular=False)
                names = list(entries.iterdir())
                if len(names) != 1 or not names[0].name.endswith('.conf'):
                    raise ValueError('fresh disk must have exactly one boot entry')
                entry = config_lines(read(esp, names[0].relative_to(esp)))
                match = re.fullmatch(r'/EFI/Linux/bootc/bootc_composefs-([0-9a-f]{128})\.efi',
                                     entry.get('efi', ''))
                if not match:
                    raise ValueError('boot entry does not select a sealed composefs UKI')
                digest = match.group(1)
                uki = selected_uki(esp, digest)
                data = read(esp, uki, prediction.boot.UKI_LIMIT)
                sections = prediction.boot.uki_sections(data, 'aarch64')
                if not (root / 'composefs').is_dir() or not (root / 'state/deploy').is_dir():
                    raise ValueError('installed root lacks composefs repository or deployment state')
                after = os.fstat(fd)
                if ((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns,
                     before.st_ctime_ns) !=
                    (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns,
                     after.st_ctime_ns)):
                    raise ValueError('disk changed during preflight')
                return {'schema': 1, 'purpose': 'tilo-disk-boot-preflight',
                    'disk': str(path), 'size_bytes': before.st_size,
                    'partition_table': 'gpt', 'sector_size': 512,
                    'partitions': [
                        {'name': 'EFI-SYSTEM', 'type': 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b',
                         'filesystem': 'vfat', 'read_only_mount_verified': True},
                        {'name': 'root', 'type': '7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab',
                         'filesystem': 'btrfs', 'read_only_mount_verified': True}],
                    'uki': {'path': uki, 'architecture': 'aarch64',
                            'sha256': hashlib.sha256(data).hexdigest(),
                            'sections': sorted(sections)},
                    'composefs_digest': digest, 'vm_boot_verified': False}
        finally:
            os.close(fd)
    finally:
        os.close(parent)


def audit_material(root, esp, uki, digest, architecture):
    etc = hardware.trusted_path(root, Path(f'state/deploy/{digest}/etc'), regular=False)
    var = hardware.trusted_path(root, Path('state/os/default/var'), regular=False)
    return material.audit_installed_material(etc, var, esp, esp / uki, architecture=architecture)


def verify_loaders(source_root, esp, public_bundle, manifest_sha256, architecture):
    """bootctl selects the .efi.signed companion; the original stays unsigned."""
    relative = Path('usr/lib/systemd/boot/efi/systemd-bootaa64.efi')
    original = hardware.trusted_path(source_root, relative, regular=True)
    signed = hardware.trusted_path(source_root, Path(str(relative) + '.signed'), regular=True)
    metadata = prediction.boot.trust.validate(public_bundle, manifest_sha256)
    evidence = prediction.boot.loader.verify(original, signed, public_bundle / 'efi-signing.crt',
        metadata['identities']['efi-signing.crt'], architecture)
    for installed in ('EFI/systemd/systemd-bootaa64.efi', 'EFI/BOOT/BOOTAA64.EFI'):
        data = read(esp, installed, 8*1024*1024)
        if hashlib.sha256(data).hexdigest() != evidence['signed_sha256']:
            raise ValueError('installed bootloader differs from verified signed companion')
    return evidence


def profile_cmdline(data, profile, generation, digest):
    prediction.boot.validate_cmdline(data, generation, digest)
    tokens = data.rstrip(b'\0').decode('ascii').split()
    install = [word for word in tokens if word.split('=', 1)[0] == 'tilo.install']
    expected = ['tilo.install'] if profile == 'installer-media' else []
    if profile not in ('installer-media', 'installed-target') or install != expected:
        raise ValueError('UKI installer activation differs from explicit boot profile')


def source_boot_selection(root, boot, profile, live_path, architecture, generation, digest):
    """Use fresh prediction/EFI evidence, never accept a caller's arbitrary EFI."""
    target_data = read(root, prediction.TARGET, prediction.boot.UKI_LIMIT)
    if hashlib.sha256(target_data).hexdigest() != boot['target_uki_sha256']:
        raise ValueError('source target changed after prediction verification')
    target_sections = prediction.boot.uki_sections(target_data, architecture)
    profile_cmdline(target_sections['.cmdline'], 'installed-target', generation, digest)
    if profile == 'installed-target':
        if live_path is not None:
            raise ValueError('normal target verification cannot select a live UKI')
        return {'profile': profile, 'source_path': prediction.TARGET.as_posix(),
                'sha256': boot['target_uki_sha256'], 'pcr_prediction_applies': True}
    if profile != 'installer-media' or not isinstance(live_path, str):
        raise ValueError('installer media requires an explicit authenticated live UKI path')
    relative = Path(live_path)
    if (relative.is_absolute() or '..' in relative.parts or relative.as_posix() != live_path
            or relative.parts[:3] != ('boot', 'EFI', 'Linux') or relative.suffix != '.efi'):
        raise ValueError('live UKI must be a canonical source boot/EFI/Linux path')
    rows = [row for row in boot['signed_ukis'] if row['path'] == live_path]
    if len(rows) != 1:
        raise ValueError('selected live UKI lacks fresh authenticated source evidence')
    data = read(root, relative, prediction.boot.UKI_LIMIT)
    if hashlib.sha256(data).hexdigest() != rows[0]['sha256']:
        raise ValueError('source live UKI changed after signature verification')
    profile_cmdline(prediction.boot.uki_sections(data, architecture)['.cmdline'], profile, generation, digest)
    return {'profile': profile, 'source_path': live_path, 'sha256': rows[0]['sha256'],
            'pcr_prediction_applies': False}


def require_private_namespace():
    if os.stat('/proc/self/ns/mnt').st_ino == os.stat('/proc/1/ns/mnt').st_ino:
        raise ValueError('requires a separate mount namespace')
    mounts = Path('/proc/self/mountinfo').read_text().splitlines()
    if any('shared:' in line.split(' - ')[0] for line in mounts):
        raise ValueError('mount namespace must have private propagation')


def _assert_digest_absent(paths, forbidden):
    scanned = 0
    for root in paths:
        for directory, names, files in os.walk(root, followlinks=False):
            for name in files:
                path = Path(directory) / name
                info = path.lstat()
                if not stat.S_ISREG(info.st_mode):
                    continue
                scanned += 1
                if installer_bootc_helper.sha256_file(path) == forbidden:
                    raise ValueError('installer helper leaked into installed mutable state or ESP')
    return scanned


def verify_installer_helper_isolation(creation, source_root, installed_root,
                                      mutable_paths, esp, architecture):
    helper = creation.get('installer_bootc_helper')
    if not isinstance(helper, dict) or set(helper) != {'artifact', 'source_binary', 'runtime'}:
        raise ValueError('creation receipt lacks installer helper isolation evidence')
    artifact, original = helper['artifact'], helper['source_binary']
    expected_artifact = {
        'schema': 1, 'purpose': installer_bootc_helper.PURPOSE,
        'architecture': architecture,
        'bootc_version': installer_bootc_helper.BOOTC_VERSION,
        'source_tag': installer_bootc_helper.SOURCE_TAG,
        'source_commit': installer_bootc_helper.SOURCE_COMMIT,
        'source_cargo_lock_sha256': installer_bootc_helper.SOURCE_CARGO_LOCK_SHA256,
        'patch_sha256': installer_bootc_helper.PATCH_SHA256,
        'rust_toolchain': installer_bootc_helper.RUST_TOOLCHAIN,
        'build_image': installer_bootc_helper.BUILD_IMAGE,
        'build_target': installer_bootc_helper.TARGETS[architecture],
        'build_command': installer_bootc_helper.BUILD_COMMAND,
    }
    if (not isinstance(artifact, dict)
            or set(artifact) != installer_bootc_helper.RECEIPT_KEYS
            or any(artifact.get(key) != value for key, value in expected_artifact.items())
            or not isinstance(original, dict)
            or artifact.get('binary_sha256') == original.get('sha256')):
        raise ValueError('creation receipt has invalid installer helper identities')
    helper_identity = {'architecture': architecture,
                       'sha256': artifact['binary_sha256'],
                       'build_id': artifact['binary_build_id']}
    runtime = helper['runtime']
    if (not isinstance(runtime, dict) or runtime.get('schema') != 1
            or runtime.get('purpose') != 'tilo-installer-bootc-runtime'
            or runtime.get('binary') != helper_identity
            or not isinstance(runtime.get('mount'), dict)
            or runtime['mount'].get('mount_point') != '/run/tilo-installer-bootc'
            or runtime['mount'].get('read_only') is not True):
        raise ValueError('creation receipt lacks bound installer helper runtime evidence')
    fresh_source = installer_bootc_helper.binary_identity(
        source_root / 'usr/bin/bootc', architecture)
    installed = installer_bootc_helper.binary_identity(
        installed_root / 'usr/bin/bootc', architecture)
    if fresh_source != original or installed != original:
        raise ValueError('installed bootc differs from the original source binary')
    if installed['sha256'] == artifact['binary_sha256']:
        raise ValueError('installer helper replaced the installed bootc')
    scanned = _assert_digest_absent([*mutable_paths, esp], artifact['binary_sha256'])
    return {'source_binary': original, 'installed_binary': installed,
            'helper_sha256': artifact['binary_sha256'],
            'mutable_and_esp_files_scanned': scanned, 'helper_absent': True}


def verify(args):
    if os.getuid() != 0 or os.geteuid() != 0 or args.architecture != 'aarch64':
        raise ValueError('requires root on the native ARM verification guest')
    import platform
    if platform.machine() != args.architecture:
        raise ValueError('requires native execution')
    require_private_namespace()
    creation_bytes=args.creation_receipt.read_bytes()
    receipt=json.loads(creation_bytes)
    copied=json.loads(args.copy_receipt.read_text()) if args.copy_receipt else None
    access=copied_disk_receipt(args.disk,creation_bytes,receipt,copied) if copied else receipt
    with source.verified_source(args) as verified, open_created_disk(args.disk, access) as fd:
        if receipt.get('source') != verified.receipt:
            raise ValueError('creation source differs from fresh source verification')
        boot = prediction.verify_prediction(verified.root, verified.public_bundle, args.manifest_sha256,
            args.architecture, args.generation, args.composefs_digest, args.systemd_version)
        selected_boot = source_boot_selection(verified.root, boot, args.boot_profile, args.source_live_uki,
            args.architecture, args.generation, args.composefs_digest)
        with tempfile.TemporaryDirectory(prefix='tilo-installed-check-') as directory, ExitStack() as cleanup:
            scratch = Path(directory)
            loop = command(['/usr/sbin/losetup', '--find', '--show', '--read-only', '--partscan',
                            f'/proc/self/fd/{fd}'], pass_fds=(fd,)).decode().strip()
            if not re.fullmatch('/dev/loop[0-9]+', loop):
                raise ValueError('unexpected allocated loop identity')
            cleanup.callback(command, ['/usr/sbin/losetup', '--detach', loop])
            cleanup.enter_context(loop_partition_nodes(loop))
            if command(['/usr/sbin/blockdev', '--getro', loop]).strip() != b'1':
                raise ValueError('loop is not read-only')
            parts = partitions(json.loads(command(['/usr/sbin/sfdisk', '--json', loop])), loop)
            esp, root, view = [scratch / name for name in ('esp', 'root', 'view')]
            for device, destination, kind, options in zip(parts, (esp, root), ('vfat', 'btrfs'),
                    ('ro,nodev,nosuid,noexec', 'ro,nodev,nosuid,noexec,nologreplay')):
                destination.mkdir()
                command(['/usr/bin/mount', '-t', kind, '-o', options, device, str(destination)])
                cleanup.callback(command, ['/usr/bin/umount', str(destination)])
                readonly_mount(destination, kind)
            target_materialization = None
            if args.boot_profile == 'installer-media':
                target_materialization = verify_materialized_target(verified.root, esp, boot, args.generation)
            uki = selected_uki(esp, args.composefs_digest, target_materialized=target_materialization is not None)
            data = read(esp, uki, prediction.boot.UKI_LIMIT)
            if hashlib.sha256(data).hexdigest() != selected_boot['sha256']:
                raise ValueError('installed selected UKI differs from verified source')
            sections = prediction.boot.uki_sections(data, args.architecture)
            profile_cmdline(sections['.cmdline'], args.boot_profile, args.generation, args.composefs_digest)
            command(['/usr/local/bin/tilo-crypto', 'verify-efi',
                     str(verified.public_bundle / 'efi-signing.crt'), str(esp / uki)])
            loader_receipt = verify_loaders(verified.root, esp, verified.public_bundle,
                                           args.manifest_sha256, args.architecture)
            deploy = f'state/deploy/{args.composefs_digest}'
            origin_identity(read(root, f'{deploy}/{args.composefs_digest}.origin'), args.target,
                            args.source.split('@')[1], sections)
            repo = hardware.trusted_path(root, Path('composefs'), regular=False)
            cfs = ['/usr/bin/bootc', 'internals', 'cfs', '--', '--repo', str(repo), '--hash', 'sha512', '--require-verity', '--no-upgrade']
            command([*cfs, 'oci', 'fsck', '@' + args.source.split('@')[1]], timeout=3600)
            manifest = json.loads(command([*cfs, 'oci', 'inspect', '--manifest', '@' + args.source.split('@')[1]]))
            if manifest['config']['digest'] != verified.image_id:
                raise ValueError('installed OCI config identity differs')
            view.mkdir()
            command([*cfs, 'mount', '--fuse', 'no', args.composefs_digest, str(view)])
            cleanup.callback(command, ['/usr/bin/umount', str(view)])
            mounted = readonly_mount(view)
            profile = source.mounted_identity(view, args.target, args.manifest_sha256, args.architecture)
            if profile != verified.receipt['hardware_profile'] or read(view, 'usr/lib/tilo/generation').strip() != str(args.generation).encode():
                raise ValueError('installed generation or hardware profile differs')
            policy = hardware.development_policy(hardware.development_target(args.target, args.architecture)['repository'])
            if json.loads(read(root, f'{deploy}/etc/containers/policy.json')) != policy:
                raise ValueError('effective mutable container signature policy differs')
            var_link = hardware.trusted_path(root, Path(deploy), regular=False) / 'var'
            shared_var = hardware.trusted_path(root, Path('state/os/default/var'), regular=False)
            if not var_link.is_symlink() or var_link.resolve(strict=True) != shared_var:
                raise ValueError('deployment var does not bind expected shared state')
            helper_isolation = verify_installer_helper_isolation(
                receipt, verified.root, view,
                [hardware.trusted_path(root, Path(deploy), regular=False), shared_var],
                esp, args.architecture)
            material_receipt = audit_material(root, esp, uki, args.composefs_digest, args.architecture)
    return {'schema': 1, 'purpose': 'tilo-installed-boot-and-root-verification',
        'source': verified.receipt, 'boot': boot, 'file_identity': access['file_identity'],
        'copy_receipt': copied,
        'selected_boot': selected_boot,
        'target_materialization': target_materialization,
        'checks': {'read_only_disk': True, 'disk_receipt_binding': True,
                   'selected_uki_matches_source': True,
                   'installed_origin': True, 'verity_mounted_root': True, 'effective_policy': True,
                   'introduced_private_material_policy': True,
                   'installer_bootc_isolation': True},
        'material_audit': material_receipt,
        'installer_bootc_isolation': helper_isolation,
        'loader': loader_receipt,
        'composefs_mount_type': mounted['fstype'], 'installed_bytes_verified': False,
        'remaining': ['independent source OCI layer private-material audit receipt', 'VM boot acceptance'],
        'vm_boot_verified': False, 'installable': False}


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--preflight-disk':
        print(json.dumps(preflight_disk(Path(sys.argv[2])), sort_keys=True))
        return
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'target', 'architecture', 'revision', 'manifest-sha256', 'composefs-digest', 'systemd-version'):
        parser.add_argument('--' + name, required=True)
    for name in ('public-bundle', 'disk', 'creation-receipt'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--copy-receipt', type=Path)
    parser.add_argument('--generation', type=int, required=True)
    parser.add_argument('--boot-profile', choices=('installed-target', 'installer-media'), default='installed-target')
    parser.add_argument('--source-live-uki', help='installer-media only: exact source boot/EFI/Linux/*.efi path')
    parser.add_argument('--offline-key-signature', action='store_true')
    print(json.dumps(verify(parser.parse_args()), sort_keys=True))


if __name__ == '__main__':
    main()
