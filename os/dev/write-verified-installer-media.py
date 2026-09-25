#!/usr/bin/env python3
"""Plan and verify a physical Windows write of accepted Tilo installer media.

Planning is read-only.  Writing requires the generated plan and its exact
confirmation phrase; the selected disk is re-observed before and after it is
taken offline.  Internal, boot, system, read-only and identity-less disks are
always rejected.
"""
import argparse
import base64
from contextlib import contextmanager
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import stat
import tarfile
from datetime import datetime, timezone


SCHEMA_PLAN = 'tilo.windows-installer-media-plan.v1'
SCHEMA_EVIDENCE = 'tilo.windows-installer-media-write.v1'
ACCEPTANCE_PURPOSE = 'tilo-arm-installer-prepared-alongside-acceptance'
HARDWARE_ACCEPTANCE_PURPOSE = 'unsigned Lenovo 83ED ARM64 development installer media'
ACCEPTED_MEDIA_BYTES = 64 * 1024**3
REMOVABLE_BUSES = {'USB', 'SD', 'MMC'}
CHUNK = 8 * 1024 * 1024


def fail(message):
    raise ValueError(message)


def command(argv, *, timeout=120):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        detail = (result.stderr or result.stdout).strip()
        fail(f'{Path(argv[0]).name} failed (exit {result.returncode}): {detail}')
    return result.stdout


def powershell(script, *, timeout=120):
    return command(['powershell.exe', '-NoLogo', '-NoProfile', '-NonInteractive',
                    '-ExecutionPolicy', 'Bypass', '-Command', script], timeout=timeout)


def require_windows():
    if platform.system() != 'Windows':
        fail('physical media handoff must run in native Windows, not WSL')


def one_or_many(value):
    if value is None:
        return []
    return value if isinstance(value, list) else [value]


def disk_inventory():
    script = r'''$ErrorActionPreference='Stop'
@(
  Get-Disk | ForEach-Object {
    [PSCustomObject]@{
      Number=[int]$_.Number; FriendlyName=[string]$_.FriendlyName
      UniqueId=[string]$_.UniqueId; Size=[long]$_.Size; BusType=[string]$_.BusType
      IsBoot=[bool]$_.IsBoot; IsSystem=[bool]$_.IsSystem
      IsOffline=[bool]$_.IsOffline; IsReadOnly=[bool]$_.IsReadOnly
      OperationalStatus=@($_.OperationalStatus | ForEach-Object {[string]$_})
      PartitionStyle=[string]$_.PartitionStyle
    }
  }
) | ConvertTo-Json -Depth 4 -Compress'''
    try:
        parsed = json.loads(powershell(script))
    except json.JSONDecodeError as error:
        fail(f'Get-Disk returned invalid JSON: {error}')
    rows = one_or_many(parsed)
    if not rows:
        fail('Get-Disk returned no disks')
    return rows


def partition_inventory(number):
    script = rf'''$ErrorActionPreference='Stop'
Update-HostStorageCache
$disk=Get-Disk -Number {int(number)} -ErrorAction Stop
$parts=@(Get-Partition -DiskNumber {int(number)} -ErrorAction SilentlyContinue | ForEach-Object {{
  [PSCustomObject]@{{
    PartitionNumber=[int]$_.PartitionNumber; Offset=[long]$_.Offset; Size=[long]$_.Size
    GptType=[string]$_.GptType; Guid=[string]$_.Guid; Type=[string]$_.Type
    DriveLetter=[string]$_.DriveLetter; IsBoot=[bool]$_.IsBoot
    IsSystem=[bool]$_.IsSystem; IsHidden=[bool]$_.IsHidden
    IsReadOnly=[bool]$_.IsReadOnly
  }}
}})
[PSCustomObject]@{{PartitionStyle=[string]$disk.PartitionStyle; Partitions=$parts}} |
  ConvertTo-Json -Depth 5 -Compress'''
    try:
        return json.loads(powershell(script))
    except json.JSONDecodeError as error:
        fail(f'partition read-back returned invalid JSON: {error}')


def normalize_model(value):
    return ' '.join(str(value or '').split()).casefold()


def stable_disk(row):
    return {
        'number': int(row.get('Number', -1)),
        'model': ' '.join(str(row.get('FriendlyName') or '').split()),
        'unique_id': str(row.get('UniqueId') or '').strip(),
        'size_bytes': int(row.get('Size', -1)),
        'bus_type': str(row.get('BusType') or '').upper(),
        'is_boot': bool(row.get('IsBoot')),
        'is_system': bool(row.get('IsSystem')),
        'is_offline': bool(row.get('IsOffline')),
        'is_read_only': bool(row.get('IsReadOnly')),
        'operational_status': [str(v) for v in one_or_many(row.get('OperationalStatus'))],
        'partition_style': str(row.get('PartitionStyle') or ''),
    }


def find_disk(rows, number):
    found = [stable_disk(row) for row in rows if int(row.get('Number', -1)) == number]
    if len(found) != 1:
        fail(f'PhysicalDrive{number} was not observed exactly once')
    return found[0]


def admit_target(disk, *, image_size, excluded_disk_numbers=()):
    if disk['bus_type'] not in REMOVABLE_BUSES:
        fail('target is not attached through an allowed removable-media bus (USB, SD or MMC)')
    if disk['is_boot'] or disk['is_system']:
        fail('target is a Windows boot or system disk')
    if disk['is_read_only']:
        fail('target disk is read-only')
    if disk['is_offline']:
        fail('target disk must begin online so this command owns the offline transition')
    if 'Online' not in disk['operational_status']:
        fail('target disk is not operationally online')
    if not disk['unique_id'] or not disk['model']:
        fail('target lacks a stable hardware/media identity or model')
    if disk['number'] in set(excluded_disk_numbers):
        fail('target contains an input, plan or evidence path')
    if disk['size_bytes'] < image_size:
        fail(f'target has {disk["size_bytes"]} bytes but accepted media requires {image_size}')
    if disk['size_bytes'] <= 0:
        fail('target reports an invalid capacity')
    return disk


def same_device(expected, current, *, allow_offline_change=False):
    keys = ('number', 'unique_id', 'size_bytes', 'bus_type', 'is_boot', 'is_system',
            'is_read_only')
    if any(expected[key] != current[key] for key in keys):
        return False
    if normalize_model(expected['model']) != normalize_model(current['model']):
        return False
    return allow_offline_change or expected['is_offline'] == current['is_offline']


def confirmation_for(target, identity):
    fingerprint = hashlib.sha256(target['unique_id'].encode()).hexdigest()[:12]
    return f'ERASE PhysicalDrive{target["number"]} {fingerprint} {identity["sha256"][:12]}'


def load_acceptance(path):
    if path.is_symlink():
        fail('acceptance receipt must be a regular, non-symlink file')
    path = path.resolve(strict=True)
    if not path.is_file():
        fail('acceptance receipt must be a regular, non-symlink file')
    raw = path.read_bytes()
    try:
        receipt = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f'acceptance receipt is invalid JSON: {error}')
    if receipt.get('purpose') == HARDWARE_ACCEPTANCE_PURPOSE:
        return load_hardware_acceptance(path, receipt, raw)
    media = receipt.get('raw_media') or {}
    archive = receipt.get('cycle_evidence_archive') or {}
    required = set(archive.get('required_files') or [])
    required_cycle = {'verdict.txt', 'exit-status.txt', 'boot-timing.txt',
                      'alongside-preservation-before-target-boot.txt',
                      '07-target-greeter.png', '15-desktop-config.png'}
    if (receipt.get('schema') != 1 or receipt.get('purpose') != ACCEPTANCE_PURPOSE
            or receipt.get('cold_boot_verified') is not True
            or receipt.get('prepared_alongside_verified') is not True
            or receipt.get('creator_and_verifier_released') is not True
            or receipt.get('workers_retained_for_followup') is not True
            or not re.fullmatch(r'[0-9a-f]{40}', str(receipt.get('helper_revision') or ''))
            or not re.fullmatch(r'[^\s@]+@sha256:[0-9a-f]{64}', str(receipt.get('source') or ''))
            or not re.fullmatch(r'[0-9a-f]{64}', str(receipt.get('finalized_verification_sha256') or ''))
            or not required_cycle.issubset(required)):
        fail('receipt does not prove completed independent ARM installer acceptance')
    digest = str(media.get('sha256') or '')
    size = media.get('size')
    if not re.fullmatch(r'[0-9a-f]{64}', digest) or size != ACCEPTED_MEDIA_BYTES:
        fail('acceptance receipt has an invalid raw-media identity')
    archive_name = archive.get('path')
    archive_digest = str(archive.get('sha256') or '')
    archive_bytes = archive.get('bytes')
    if (not isinstance(archive_name, str) or Path(archive_name).name != archive_name
            or not re.fullmatch(r'[0-9a-f]{64}', archive_digest)
            or not isinstance(archive_bytes, int) or archive_bytes <= 0):
        fail('acceptance receipt has invalid cycle-evidence identity')
    archive_path = path.parent / archive_name
    finalized_path = path.parent / 'finalized-verification.json'
    cycle_path = path.parent / str(receipt.get('cycle_evidence_file') or '')
    raw_identity_path = path.parent / 'raw-identity.json'
    for companion in (archive_path, finalized_path, cycle_path, raw_identity_path):
        if companion.is_symlink() or not companion.is_file():
            fail(f'acceptance companion is missing or unsafe: {companion.name}')
    if archive_path.stat().st_size != archive_bytes or file_sha256(archive_path) != archive_digest:
        fail('cycle evidence archive differs from the accepted identity')
    if file_sha256(finalized_path) != receipt['finalized_verification_sha256']:
        fail('finalized verification differs from the accepted identity')
    try:
        cycle = json.loads(cycle_path.read_bytes())
        raw_identity = json.loads(raw_identity_path.read_bytes())
    except json.JSONDecodeError as error:
        fail(f'acceptance companion is invalid JSON: {error}')
    if (cycle.get('verdict') != 'PASS\n' or cycle.get('exit_status') != '0\n'
            or len(str(cycle.get('conversion_sha256') or '').splitlines()) != 2):
        fail('cycle evidence does not prove a passing install and cold boot')
    if raw_identity != {'size': size, 'sha256': digest}:
        fail('raw identity companion differs from acceptance')
    with tarfile.open(archive_path, 'r:') as bundle:
        members = {}
        for member in bundle:
            parts = Path(member.name).parts
            if member.name.startswith('/') or '..' in parts or not (member.isfile() or member.isdir()):
                fail('cycle evidence archive contains an unsafe member')
            if member.isfile():
                members[Path(member.name).name] = member
        if not required_cycle.issubset(members):
            fail('cycle evidence archive lacks required cold-boot proof')
        verdict = bundle.extractfile(members['verdict.txt']).read()
        exit_status = bundle.extractfile(members['exit-status.txt']).read()
        if verdict != b'PASS\n' or exit_status != b'0\n':
            fail('cycle evidence archive records a failed acceptance')
    return path, receipt, {'size': size, 'sha256': digest}, hashlib.sha256(raw).hexdigest()


def load_hardware_acceptance(path, receipt, raw):
    """Validate the profile-specific handoff emitted after a hardware overlay.

    This receipt describes the final USB bytes directly instead of referring to
    the older generic VM-cycle archive.  Keep the requirements explicit so a
    descriptive build manifest cannot accidentally authorize a disk write.
    """
    artifact = receipt.get('artifact') or {}
    hardware = receipt.get('hardware') or {}
    source = receipt.get('os_source') or {}
    uki = receipt.get('uki') or {}
    verification = receipt.get('verification') or {}
    digest = str(artifact.get('sha256') or '')
    size = artifact.get('bytes')
    sha256 = lambda value: bool(re.fullmatch(r'[0-9a-f]{64}', str(value or '')))
    verity = lambda value: bool(re.fullmatch(r'[0-9a-f]{128}', str(value or '')))
    immutable = str(source.get('immutable_reference') or '')
    live_contracts = set(uki.get('live_cmdline_contracts') or [])
    target_contracts = set(uki.get('target_cmdline_contracts') or [])
    mount_contract = 'systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec'
    if (receipt.get('schema') != 1
            or artifact.get('partition_table') != 'GPT'
            or size != ACCEPTED_MEDIA_BYTES or not sha256(digest)
            or hardware.get('architecture') != 'aarch64'
            or hardware.get('profile') != 'lenovo-83ed'
            or not str(hardware.get('model') or '').startswith('Lenovo Yoga Slim 7')
            or hardware.get('devicetree') != 'qcom/x1e80100-lenovo-yoga-slim7x.dtb'
            or not sha256(hardware.get('devicetree_sha256'))
            or not re.fullmatch(r'[^\s@]+@sha256:[0-9a-f]{64}', immutable)
            or not str(source.get('signature') or '').startswith('verified ')
            or not verity(source.get('composefs_verity'))
            or not all(sha256(uki.get(field)) for field in
                       ('base_sha256', 'firmware_initramfs_sha256', 'live_sha256',
                        'source_uki_before_live_sha256', 'target_sha256'))
            or {'root=tilo-auto', 'tilo.install', mount_contract} - live_contracts
            or {'root=tilo-auto', mount_contract} - target_contracts
            or 'tilo.install' not in set(uki.get('target_excludes') or [])
            or verification.get('physical_disks_touched') is not False
            or verification.get('usb_touched') is not False
            or not str(verification.get('proven_base') or '').startswith('clean local install')
            or 'clean fsck.fat' not in str(verification.get('esp') or '')
            or 'matched pinned SHA-256' not in str(verification.get('esp_readback') or '')
            or 'removed' not in str(verification.get('identity_scrub') or '')):
        fail('receipt does not prove accepted Lenovo 83ED installer media')
    return path, receipt, {'size': size, 'sha256': digest}, hashlib.sha256(raw).hexdigest()


def file_sha256(path):
    digest = hashlib.sha256()
    with path.open('rb', buffering=0) as stream:
        for chunk in iter(lambda: stream.read(CHUNK), b''):
            digest.update(chunk)
    return digest.hexdigest()


def hash_stream(stream, length):
    digest = hashlib.sha256()
    remaining = length
    while remaining:
        chunk = stream.read(min(CHUNK, remaining))
        if not chunk:
            fail('input ended before its declared length')
        digest.update(chunk)
        remaining -= len(chunk)
    return digest.hexdigest()


def verify_image(path, identity):
    if path.is_symlink():
        fail('installer image must be a regular, non-symlink file')
    path = path.resolve(strict=True)
    if not path.is_file():
        fail('installer image must be a regular, non-symlink file')
    if path.stat().st_size != identity['size']:
        fail('installer image length differs from accepted raw-media identity')
    with path.open('rb', buffering=0) as stream:
        digest = hash_stream(stream, identity['size'])
    if digest != identity['sha256']:
        fail('installer image digest differs from accepted raw-media identity')
    return path


@contextmanager
def exclusive_source(path):
    """Keep the verified source locked against replacement and modification."""
    if platform.system() != 'Windows':
        with path.open('rb', buffering=0) as stream:
            yield stream
        return
    import msvcrt
    create = ctypes.windll.kernel32.CreateFileW
    create.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32,
                       ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32,
                       ctypes.c_void_p]
    create.restype = ctypes.c_void_p
    handle = create(str(path), 0x80000000, 0, None, 3, 0x08000000, None)
    invalid = ctypes.c_void_p(-1).value
    if handle in (None, invalid):
        fail(f'could not exclusively lock installer image (Windows error {ctypes.get_last_error()})')
    try:
        fd = msvcrt.open_osfhandle(handle, os.O_RDONLY | os.O_BINARY)
    except Exception:
        ctypes.windll.kernel32.CloseHandle(handle)
        raise
    with os.fdopen(fd, 'rb', buffering=0) as stream:
        yield stream


def disk_for_local_path(path):
    resolved = path.resolve(strict=False)
    drive = resolved.drive
    if not re.fullmatch(r'[A-Za-z]:', drive):
        fail(f'path must be on a local Windows volume: {resolved}')
    letter = drive[0].upper()
    script = (f"$ErrorActionPreference='Stop'; "
              f"[int](Get-Partition -DriveLetter {letter} -ErrorAction Stop).DiskNumber")
    value = powershell(script).strip()
    if not re.fullmatch(r'\d+', value):
        fail(f'could not bind {resolved} to one physical disk')
    return int(value)


def atomic_json(path, value):
    path = path.resolve(strict=False)
    if not path.parent.is_dir():
        fail(f'output directory does not exist: {path.parent}')
    encoded = (json.dumps(value, indent=2, sort_keys=True) + '\n').encode()
    fd, temporary = tempfile.mkstemp(prefix='.' + path.name + '-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            output.write(encoded)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def reserve_evidence(path, plan_path, acceptance_sha):
    path = path.resolve(strict=False)
    if not path.parent.is_dir():
        fail(f'evidence directory does not exist: {path.parent}')
    value = {
        'schema': SCHEMA_EVIDENCE,
        'status': 'in-progress',
        'started_at_utc': datetime.now(timezone.utc).isoformat(),
        'plan': str(plan_path),
        'acceptance_sha256': acceptance_sha,
    }
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_BINARY
    try:
        fd = os.open(path, flags, 0o600)
    except FileExistsError:
        fail('evidence path already exists; review and move it before a new write')
    with os.fdopen(fd, 'wb') as output:
        output.write((json.dumps(value, indent=2, sort_keys=True) + '\n').encode())
        output.flush(); os.fsync(output.fileno())


def plan(args):
    acceptance_path, _, identity, acceptance_sha = load_acceptance(args.acceptance)
    image = verify_image(args.image, identity)
    plan_path = args.output.resolve(strict=False)
    evidence_path = args.evidence.resolve(strict=False)
    if plan_path == evidence_path or plan_path in (image, acceptance_path) or evidence_path in (image, acceptance_path):
        fail('image, acceptance, plan and evidence paths must be distinct')
    if plan_path.exists() or evidence_path.exists():
        fail('plan and evidence outputs must not already exist')
    path_disks = {disk_for_local_path(path) for path in
                  (image, acceptance_path, plan_path, evidence_path)}
    selected = admit_target(find_disk(disk_inventory(), args.disk_number),
                            image_size=identity['size'], excluded_disk_numbers=path_disks)
    confirmation = confirmation_for(selected, identity)
    document = {
        'schema': SCHEMA_PLAN,
        'created_at_utc': datetime.now(timezone.utc).isoformat(),
        'destructive': False,
        'image': str(image),
        'acceptance': str(acceptance_path),
        'acceptance_sha256': acceptance_sha,
        'raw_media': identity,
        'target': selected,
        'evidence': str(evidence_path),
        'required_confirmation': confirmation,
    }
    atomic_json(plan_path, document)
    print(json.dumps(document, indent=2, sort_keys=True))


def load_plan(path):
    if path.is_symlink():
        fail('plan must be a regular, non-symlink file')
    path = path.resolve(strict=True)
    if not path.is_file():
        fail('plan must be a regular, non-symlink file')
    raw = path.read_bytes()
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f'plan is invalid JSON: {error}')
    if value.get('schema') != SCHEMA_PLAN or value.get('destructive') is not False:
        fail('unsupported or unsafe media plan')
    target = value.get('target') or {}
    identity = value.get('raw_media') or {}
    if (not isinstance(target.get('number'), int)
            or not re.fullmatch(r'[0-9a-f]{64}', str(identity.get('sha256') or ''))
            or not isinstance(identity.get('size'), int)):
        fail('media plan is incomplete')
    expected_confirmation = confirmation_for(target, identity)
    if value.get('required_confirmation') != expected_confirmation:
        fail('plan confirmation does not bind its target and raw-media identity')
    return path, value, hashlib.sha256(raw).hexdigest()


def require_administrator():
    script = ('([Security.Principal.WindowsPrincipal]::new('
              '[Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole('
              '[Security.Principal.WindowsBuiltInRole]::Administrator)')
    if powershell(script).strip().lower() != 'true':
        fail('run the write command from an Administrator PowerShell')


def set_offline(unique_id, number, offline):
    state = '$true' if offline else '$false'
    encoded = base64.b64encode(unique_id.encode()).decode()
    powershell("$ErrorActionPreference='Stop'; "
               f"$id=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}')); "
               f"$matches=@(Get-Disk | Where-Object {{$_.UniqueId -eq $id}}); "
               f"if ($matches.Count -ne 1 -or [int]$matches[0].Number -ne {int(number)}) "
               "{ throw 'stable disk identity is not unique at transition' }; "
               f"$matches[0] | Set-Disk -IsOffline {state} -ErrorAction Stop", timeout=60)


def raw_device(number):
    return Path(rf'\\.\PhysicalDrive{int(number)}')


def handle_identity(target):
    """Read capacity and hotplug properties from the exact retained handle."""
    if platform.system() != 'Windows' or stat.S_ISREG(os.fstat(target.fileno()).st_mode):
        here = target.tell(); target.seek(0, os.SEEK_END); length = target.tell(); target.seek(here)
        return {'size_bytes': length, 'media_removable': True, 'device_hotplug': True,
                'device_number': None, 'bus_type': None, 'vendor': '', 'product': '',
                'serial': ''}
    import msvcrt
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    ioctl = kernel.DeviceIoControl
    ioctl.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32,
                      ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32),
                      ctypes.c_void_p]
    ioctl.restype = ctypes.c_int
    handle = msvcrt.get_osfhandle(target.fileno())
    returned = ctypes.c_uint32()
    length = ctypes.c_longlong()
    if not ioctl(handle, 0x0007405C, None, 0, ctypes.byref(length), ctypes.sizeof(length),
                 ctypes.byref(returned), None):
        fail(f'cannot read exact target capacity (Windows error {ctypes.get_last_error()})')
    hotplug = (ctypes.c_ubyte * 20)()
    if not ioctl(handle, 0x002D0C14, None, 0, hotplug, len(hotplug),
                 ctypes.byref(returned), None):
        fail(f'cannot read exact target hotplug state (Windows error {ctypes.get_last_error()})')
    device_number = (ctypes.c_uint32 * 3)()
    if not ioctl(handle, 0x002D1080, None, 0, device_number, ctypes.sizeof(device_number),
                 ctypes.byref(returned), None):
        fail(f'cannot read exact target device number (Windows error {ctypes.get_last_error()})')
    query = (ctypes.c_ubyte * 12)()
    header = (ctypes.c_ubyte * 8)()
    if not ioctl(handle, 0x002D1400, query, len(query), header, len(header),
                 ctypes.byref(returned), None):
        fail(f'cannot query exact target descriptor (Windows error {ctypes.get_last_error()})')
    descriptor_size = int.from_bytes(bytes(header[4:8]), 'little')
    if descriptor_size < 36 or descriptor_size > 1024 * 1024:
        fail('exact target returned an invalid storage descriptor size')
    descriptor = (ctypes.c_ubyte * descriptor_size)()
    if not ioctl(handle, 0x002D1400, query, len(query), descriptor, descriptor_size,
                 ctypes.byref(returned), None):
        fail(f'cannot read exact target descriptor (Windows error {ctypes.get_last_error()})')
    raw = bytes(descriptor)
    def field(offset):
        start = int.from_bytes(raw[offset:offset + 4], 'little')
        if not start or start >= len(raw):
            return ''
        return raw[start:].split(b'\0', 1)[0].decode('ascii', 'replace').strip()
    bus_number = int.from_bytes(raw[28:32], 'little')
    bus_names = {7: 'USB', 12: 'SD', 13: 'MMC'}
    return {'size_bytes': int(length.value), 'media_removable': bool(hotplug[4]),
            'device_hotplug': bool(hotplug[6]), 'device_number': int(device_number[1]),
            'bus_type': bus_names.get(bus_number, f'BUS-{bus_number}'),
            'vendor': field(12), 'product': field(16), 'serial': field(24)}


def clear_and_verify_terminal_metadata(target, target_size, image_size):
    if target_size == image_size:
        return {'cleared': False, 'bytes': 0}
    length = min(4 * 1024 * 1024, target_size - image_size)
    start = target_size - length
    zero = b'\0' * min(CHUNK, length)
    target.seek(start)
    remaining = length
    while remaining:
        block = zero[:min(len(zero), remaining)]
        if target.write(block) != len(block):
            fail('target stopped while clearing terminal metadata')
        remaining -= len(block)
    target.flush(); os.fsync(target.fileno()); target.seek(start)
    if any(target.read(min(CHUNK, length - done)).strip(b'\0')
           for done in range(0, length, CHUNK)):
        fail('terminal target metadata did not read back as cleared')
    return {'cleared': True, 'offset': start, 'bytes': length,
            'sha256': hashlib.sha256(b'\0' * length).hexdigest()}


def copy_and_readback(source, target, identity):
    written_hash = hashlib.sha256()
    written = 0
    source.seek(0)
    target.seek(0)
    while written < identity['size']:
        block = source.read(min(CHUNK, identity['size'] - written))
        if not block:
            fail('installer image ended during physical write')
        view = memoryview(block)
        while view:
            count = target.write(view)
            if not count:
                fail('physical target stopped accepting bytes')
            view = view[count:]
        written_hash.update(block)
        written += len(block)
    target.flush()
    os.fsync(target.fileno())
    target.seek(0)
    readback_hash = hash_stream(target, identity['size'])
    source_hash = written_hash.hexdigest()
    if source_hash != identity['sha256']:
        fail('source changed after pre-write verification')
    if readback_hash != identity['sha256']:
        fail('physical-media read-back differs from accepted installer image')
    return {'bytes': written, 'source_sha256': source_hash, 'readback_sha256': readback_hash}


def write_and_readback(image, device, identity):
    """Testable wrapper; the physical path keeps this lock across preverification."""
    with exclusive_source(image) as source, open(device, 'r+b', buffering=0) as target:
        properties = handle_identity(target)
        if properties['size_bytes'] < identity['size']:
            fail('opened target handle is smaller than accepted media')
        if not (properties['media_removable'] or properties['device_hotplug']):
            fail('opened target handle is not removable or hotpluggable')
        return copy_and_readback(source, target, identity)


def write(args):
    plan_path, document, plan_sha = load_plan(args.plan)
    if args.confirmation != document.get('required_confirmation'):
        fail('confirmation phrase does not exactly match the reviewed plan')
    acceptance_path, _, identity, acceptance_sha = load_acceptance(Path(document['acceptance']))
    if identity != document['raw_media'] or acceptance_sha != document.get('acceptance_sha256'):
        fail('acceptance receipt changed after planning')
    image = Path(document['image'])
    if image.is_symlink():
        fail('installer image became a symlink after planning')
    image = image.resolve(strict=True)
    if not image.is_file() or image.stat().st_size != identity['size']:
        fail('installer image changed after planning')
    evidence = Path(document['evidence']).resolve(strict=False)
    excluded = {disk_for_local_path(path) for path in
                (image, acceptance_path, plan_path, evidence)}
    expected = document['target']
    current = admit_target(find_disk(disk_inventory(), expected['number']),
                           image_size=identity['size'], excluded_disk_numbers=excluded)
    if not same_device(expected, current):
        fail('selected removable disk changed after planning')
    require_administrator()
    reserve_evidence(evidence, plan_path, acceptance_sha)
    offlined = False
    primary_error = None
    result = None
    after = None
    try:
        # The source stays exclusively locked from digest verification through
        # the last byte copied; no path replacement or writer can race it.
        with exclusive_source(image) as source:
            if hash_stream(source, identity['size']) != identity['sha256']:
                fail('installer image digest differs from accepted raw-media identity')
            # Last observation before the first mutation.
            latest = admit_target(find_disk(disk_inventory(), expected['number']),
                                  image_size=identity['size'], excluded_disk_numbers=excluded)
            if not same_device(expected, latest):
                fail('selected removable disk changed immediately before write')
            # Even a timed-out/failed transition may have reached the device.
            # Mark cleanup required before invoking it; the finally path uses
            # the same unique-ID/count/number gate to return only this disk online.
            offlined = True
            set_offline(expected['unique_id'], expected['number'], True)
            offline_disk = find_disk(disk_inventory(), expected['number'])
            if not offline_disk['is_offline'] or not same_device(expected, offline_disk, allow_offline_change=True):
                fail('offline target no longer matches the reviewed removable disk')
            # Open once, then bind properties and a fresh Get-Disk observation
            # to this retained handle before the first byte is changed.
            with open(raw_device(expected['number']), 'r+b', buffering=0) as target:
                properties = handle_identity(target)
                if properties['size_bytes'] != expected['size_bytes']:
                    fail('opened target handle capacity differs from the reviewed disk')
                if properties['device_number'] != expected['number']:
                    fail('opened raw handle has a different Windows device number')
                if properties['bus_type'] != expected['bus_type']:
                    fail('opened raw handle bus differs from the reviewed disk')
                if not (properties['media_removable'] or properties['device_hotplug']):
                    fail('opened target handle is not removable or hotpluggable')
                held = find_disk(disk_inventory(), expected['number'])
                if not held['is_offline'] or not same_device(expected, held, allow_offline_change=True):
                    fail('open raw handle is no longer the reviewed removable disk')
                terminal = clear_and_verify_terminal_metadata(
                    target, properties['size_bytes'], identity['size'])
                result = copy_and_readback(source, target, identity)
    except Exception as error:
        primary_error = error
    finally:
        if offlined:
            try:
                observed = find_disk(disk_inventory(), expected['number'])
                if not same_device(expected, observed, allow_offline_change=True):
                    fail('refusing to online a different disk that reused the target number')
                set_offline(expected['unique_id'], expected['number'], False)
                after = find_disk(disk_inventory(), expected['number'])
                if after['is_offline'] or not same_device(expected, after, allow_offline_change=True):
                    fail('written removable disk did not return online with the same identity')
            except Exception as cleanup_error:
                if primary_error is None:
                    primary_error = cleanup_error
                else:
                    primary_error = RuntimeError(f'{primary_error}; cleanup also failed: {cleanup_error}')
    if primary_error is not None:
        raise primary_error
    partitions = partition_inventory(expected['number'])
    record = {
        'schema': SCHEMA_EVIDENCE,
        'status': 'complete',
        'completed_at_utc': datetime.now(timezone.utc).isoformat(),
        'accepted_vm_artifact': True,
        'plan': str(plan_path),
        'plan_sha256': plan_sha,
        'acceptance': str(acceptance_path),
        'acceptance_sha256': acceptance_sha,
        'raw_media': identity,
        'target_before': expected,
        'target_after': after,
        'write': result,
        'terminal_metadata': terminal,
        'partition_readback': partitions,
    }
    atomic_json(evidence, record)
    print(json.dumps(record, indent=2, sort_keys=True))


def inventory(args):
    rows = [stable_disk(row) for row in disk_inventory()]
    document = {
        'schema': 'tilo.windows-disk-inventory.v1',
        'observed_at_utc': datetime.now(timezone.utc).isoformat(),
        'read_only': True,
        'disks': rows,
        'eligible_removable': [row for row in rows if
            row['bus_type'] in REMOVABLE_BUSES and not row['is_boot'] and
            not row['is_system'] and not row['is_read_only'] and not row['is_offline'] and
            'Online' in row['operational_status'] and
            bool(row['unique_id']) and bool(row['model'])],
    }
    if args.output:
        atomic_json(args.output, document)
    print(json.dumps(document, indent=2, sort_keys=True))


def parser():
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest='command', required=True)
    show = commands.add_parser('inventory', help='read-only Windows disk inventory')
    show.add_argument('--output', type=Path)
    show.set_defaults(func=inventory)
    make = commands.add_parser('plan', help='verify accepted media and capture one removable target')
    make.add_argument('--image', type=Path, required=True)
    make.add_argument('--acceptance', type=Path, required=True)
    make.add_argument('--disk-number', type=int, required=True)
    make.add_argument('--evidence', type=Path, required=True)
    make.add_argument('--output', type=Path, required=True)
    make.set_defaults(func=plan)
    run = commands.add_parser('write', help='execute and read back an exact reviewed plan')
    run.add_argument('--plan', type=Path, required=True)
    run.add_argument('--confirmation', required=True)
    run.set_defaults(func=write)
    return root


def main(argv=None):
    require_windows()
    args = parser().parse_args(argv)
    args.func(args)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        raise SystemExit(f'installer-media handoff: {error}')
