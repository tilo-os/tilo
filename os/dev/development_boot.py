"""Offline development UKI checks; not PCR, disk-installation or boot proof."""
import hashlib
import importlib.util
import os
from pathlib import Path
import re
import tempfile

import hardware_profile as hardware


def _module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


trust = _module('boot_development_trust', 'development-trust.py')
loader = _module('boot_development_loader', 'verify-development-loader.py')
pe = loader.envelope
UKI_LIMIT = 512 * 1024 * 1024
TOTAL_LIMIT = 2 * 1024 * 1024 * 1024
UKI_COUNT_LIMIT = 32


def expected_inputs(architecture, generation, composefs_digest):
    if architecture not in loader.MACHINES:
        raise ValueError('unsupported UKI architecture')
    if type(generation) not in (int, str) or not re.fullmatch(r'[1-9][0-9]{0,19}', str(generation)):
        raise ValueError('generation must be a canonical positive u64')
    generation = int(generation)
    if generation > 2**64 - 1:
        raise ValueError('generation exceeds u64')
    if not isinstance(composefs_digest, str) or not re.fullmatch(r'[0-9a-f]{128}', composefs_digest):
        raise ValueError('composefs seal must be 128 lowercase hexadecimal characters')
    if composefs_digest == '0' * 128:
        raise ValueError('provisional composefs seal is not valid boot evidence')
    return generation


def uki_sections(data, architecture):
    """Parse bounded, unambiguous PE32+ geometry before using section contents."""
    if len(data) < 64 or data[:2] != b'MZ':
        raise ValueError('UKI has no DOS header')
    header = pe.u32(data, 0x3c)
    if header < 64 or data[header:header + 4] != b'PE\0\0':
        raise ValueError('UKI has no valid PE header')
    if pe.u16(data, header + 4) != loader.MACHINES[architecture]:
        raise ValueError('UKI machine differs from native architecture')
    count, optional_size = pe.u16(data, header + 6), pe.u16(data, header + 20)
    optional = header + 24
    table = optional + optional_size
    end = table + count * 40
    if not 1 <= count <= 64 or optional_size < 152 or end > len(data):
        raise ValueError('UKI has invalid optional or section headers')
    if pe.u16(data, optional) != 0x20b or pe.u16(data, optional + 68) != 10:
        raise ValueError('UKI must be a PE32+ EFI application')
    if pe.u32(data, optional + 108) < 5:
        raise ValueError('UKI has no security directory')
    certificate = pe.u32(data, optional + 144)
    payload_end = certificate or len(data)
    headers_size = pe.u32(data, optional + 60)
    image_size = pe.u32(data, optional + 56)
    if not end <= headers_size <= payload_end:
        raise ValueError('UKI section table exceeds declared headers')
    result, raw_ranges, virtual_ranges = {}, [], []
    for index in range(count):
        offset = table + index * 40
        raw_name = data[offset:offset + 8]
        name_bytes = raw_name.rstrip(b'\0')
        if not re.fullmatch(rb'\.[A-Za-z0-9_]+', name_bytes):
            raise ValueError('UKI section name is invalid')
        name = name_bytes.decode('ascii')
        if name in result:
            raise ValueError('UKI has duplicate section names')
        virtual_size, address = pe.u32(data, offset + 8), pe.u32(data, offset + 12)
        raw_size, raw = pe.u32(data, offset + 16), pe.u32(data, offset + 20)
        if raw_size:
            if raw < headers_size or raw + raw_size > payload_end:
                raise ValueError('UKI section exceeds payload or overlaps headers')
            raw_ranges.append((raw, raw + raw_size))
        loaded_size = max(virtual_size, raw_size)
        if loaded_size:
            if address < headers_size or address + loaded_size > image_size:
                raise ValueError('UKI section exceeds virtual image')
            virtual_ranges.append((address, address + loaded_size))
        if name in ('.linux', '.initrd', '.cmdline') and not 0 < virtual_size <= raw_size:
            raise ValueError('required UKI section has no complete file payload')
        result[name] = data[raw:raw + min(virtual_size, raw_size)]
    for ranges in (raw_ranges, virtual_ranges):
        ordered = sorted(ranges)
        if any(right[0] < left[1] for left, right in zip(ordered, ordered[1:])):
            raise ValueError('UKI sections overlap')
    if any(not result.get(name) for name in ('.linux', '.initrd', '.cmdline')):
        raise ValueError('UKI requires nonempty linux, initrd and cmdline sections')
    return result


def validate_cmdline(data, generation, composefs_digest):
    value = data.rstrip(b'\0')
    if not value or len(value) > 65536 or any(byte < 32 or byte > 126 for byte in value):
        raise ValueError('UKI command line is empty, oversized or contains control bytes')
    text = value.decode('ascii')
    if any(char in text for char in ('"', "'", '\\')):
        raise ValueError('quoted or escaped UKI command lines are not supported')
    tokens = text.split()
    for key, expected in (('tilo.generation', str(generation)), ('composefs', composefs_digest)):
        matches = [token for token in tokens if token.split('=', 1)[0] == key]
        if matches != [f'{key}={expected}']:
            raise ValueError(f'UKI command line must contain one exact {key}')


def _uki_paths(root):
    live = Path('boot/EFI/Linux')
    hardware.trusted_path(root, live, regular=False)
    paths, entries = [], 0
    for directory, directories, files in os.walk(root / live, followlinks=False):
        relative = Path(directory).relative_to(root)
        if len(relative.parts) > 12:
            raise ValueError('UKI tree exceeds maximum depth')
        for name in directories:
            entries += 1
            if entries > 256:
                raise ValueError('UKI tree contains too many entries')
            hardware.trusted_path(root, relative / name, regular=False)
        for name in files:
            entries += 1
            if entries > 256:
                raise ValueError('UKI tree contains too many files')
            path = relative / name
            hardware.trusted_path(root, path, regular=True)
            if name.lower().endswith('.efi'):
                paths.append(path)
    if not paths:
        raise ValueError('image has no live UKIs')
    target = Path('boot/tilo-target/tilo-target.efi')
    target_directory = root / target.parent
    if target_directory.exists() or target_directory.is_symlink():
        hardware.trusted_path(root, target.parent, regular=False)
        if (root / target).exists() or (root / target).is_symlink():
            hardware.trusted_path(root, target, regular=True)
            paths.append(target)
    if len(paths) > UKI_COUNT_LIMIT:
        raise ValueError('image contains too many UKIs')
    return sorted(paths)


def validate_mounted_boot(root, public_bundle, manifest_sha256, architecture, generation, composefs_digest):
    """Validate all live UKIs and an explicit target without executing image code."""
    generation = expected_inputs(architecture, generation, composefs_digest)
    if not isinstance(manifest_sha256, str) or not re.fullmatch(r'[0-9a-f]{64}', manifest_sha256):
        raise ValueError('independent public manifest SHA256 is required')
    root = Path(root)
    # Public trust is parsed only by the host's root-installed Rust helper,
    # never by an image-provided binary or process-controlled PATH entry.
    if not Path('/usr/local/bin/tilo-crypto').is_file():
        raise ValueError('public validation requires host tilo-crypto')
    metadata = trust.validate(public_bundle, manifest_sha256)
    receipts, total = [], 0
    with tempfile.TemporaryDirectory(prefix='tilo-boot-validation-') as temporary:
        scratch = Path(temporary)
        generation_path = hardware.trusted_path(root, Path('usr/lib/tilo/generation'), regular=True)
        raw_generation = loader.snapshot(generation_path, scratch / 'generation', 32)
        if raw_generation not in (str(generation).encode(), f'{generation}\n'.encode()):
            raise ValueError('mounted generation differs from expected generation')
        certificate = scratch / 'certificate.pem'
        loader.snapshot(Path(public_bundle) / 'efi-signing.crt', certificate, 65536)
        certificate_identity = loader.command(
            ['/usr/local/bin/tilo-crypto', 'certificate-sha256', str(certificate)]).decode().strip()
        if certificate_identity != metadata['identities']['efi-signing.crt']:
            raise ValueError('EFI certificate changed after public bundle validation')
        for relative in _uki_paths(root):
            path = hardware.trusted_path(root, relative, regular=True)
            snapshot = scratch / 'uki.efi'
            data = loader.snapshot(path, snapshot, UKI_LIMIT)
            total += len(data)
            if total > TOTAL_LIMIT:
                raise ValueError('UKI inventory exceeds total byte limit')
            sections = uki_sections(data, architecture)
            validate_cmdline(sections['.cmdline'], generation, composefs_digest)
            pe.normalized_image(str(snapshot), require_certificate=True)
            signature_count = loader.command(
                ['/usr/local/bin/tilo-crypto', 'efi-signature-count', str(snapshot)]).decode().strip()
            if not signature_count.isdigit() or int(signature_count) < 1:
                raise ValueError('UKI has no recognized Authenticode signature')
            loader.command(['/usr/local/bin/tilo-crypto', 'verify-efi',
                            str(certificate), str(snapshot)])
            receipts.append({'path': relative.as_posix(), 'sha256': hashlib.sha256(data).hexdigest(),
                             'size_bytes': len(data)})
    return {'schema': 1, 'purpose': 'tilo-development-offline-boot-validation',
            'architecture': architecture, 'generation': generation, 'composefs_digest': composefs_digest,
            'manifest_sha256': manifest_sha256, 'efi_certificate_sha256': metadata['identities']['efi-signing.crt'],
            'ukis': receipts, 'installable': False,
            'proof': {'efi_signatures': True, 'pcr_recomputed': False,
                      'composefs_installation': False, 'vm_boot': False}}
