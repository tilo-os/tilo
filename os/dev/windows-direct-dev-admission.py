#!/usr/bin/env python3
"""Explicit CI development admission shared by final source and physical copy.

Recheck live CI image/media authority, immutable backing disks, profile-only
overlays and backend-derived guest proofs. This never substitutes a signature
claim for developer evidence. Physical bounds and full readback stay in the
existing Windows-direct copy implementation.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
from types import SimpleNamespace
import uuid


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


ci = module('dev_admission_ci', 'windows-direct-dev-provenance.py')
media = module('dev_admission_media', 'forgejo-dev-installer-metadata.py')
profile = module('dev_admission_profile', 'profile-installer-qcow2.py')
require = ci.require
HEX = re.compile(r'[0-9a-f]{64}\Z')
COMPOSEFS = re.compile(r'[0-9a-f]{128}\Z')


def file_hash(path):
    require(path.is_file() and not path.is_symlink(), 'development input is redirected or missing')
    value = hashlib.sha256()
    with path.open('rb') as stream:
        while block := stream.read(4 * 1024 * 1024):
            value.update(block)
    return value.hexdigest()


def qemu_json(*args):
    output = subprocess.run(['qemu-img', *args], capture_output=True,
                            check=True, timeout=60).stdout
    require(len(output) <= 32 * 1024 * 1024, 'QEMU metadata output is oversized')
    return json.loads(output)


def expected_tail(handoff):
    payload = profile.handoff_payload(handoff)
    require({path.relative_to(handoff).as_posix() for path in handoff.rglob('*') if path.is_file()}
            == profile.HANDOFF_FILES, 'physical development handoff requires both Wi-Fi and SSH key')
    extra = (len(payload) + 511) // 512 * 512 + profile.HANDOFF_HEADER
    header = (b'TILO_SETUP_HANDOFF_V1\n' + hashlib.sha256(payload).hexdigest().encode()
              + b'\n' + str(len(payload)).encode() + b'\n').ljust(profile.HANDOFF_HEADER, b'\0')
    return bytes(extra - profile.HANDOFF_HEADER - len(payload)) + payload + header + bytes(65536)


def checked_profile(path, expected_base_hash, expected_profile_hash, handoff):
    require(path is not None and HEX.fullmatch(expected_base_hash)
            and HEX.fullmatch(expected_profile_hash), 'development medium pins are invalid')
    chain = qemu_json('info', '--output=json', '--backing-chain', '-f', 'qcow2', str(path))
    require(type(chain) is list and len(chain) == 2
            and all(row.get('format') == 'qcow2' for row in chain),
            'development medium requires exactly one profile overlay over its CI disk')
    base = Path(chain[1]['filename'])
    size = chain[1].get('virtual-size')
    require(type(size) is int and size > 65536
            and type(chain[0].get('virtual-size')) is int
            and size + 65536 <= chain[0]['virtual-size'] <= size + 1024 * 1024,
            'development profile has an unexpected tail size')
    require(file_hash(base) == expected_base_hash and file_hash(path) == expected_profile_hash,
            'development medium differs from CI or the tested profile overlay')
    tail = expected_tail(handoff)
    require(chain[0]['virtual-size'] == size + len(tail)
            and profile.read_tail(path, size, len(tail)) == tail,
            'development profile is not the authorized Wi-Fi/SSH-only handoff')
    mapping = qemu_json('map', '--output=json', '-f', 'qcow2', str(path))
    require(type(mapping) is list and len(mapping) <= 262144, 'invalid profile extent inventory')
    for extent in mapping:
        if extent.get('depth') != 0 or not (extent.get('data') or extent.get('present')):
            continue
        start, length = extent.get('start'), extent.get('length')
        require(type(start) is int and type(length) is int and length > 0
                and size - 65536 <= start and start + length <= chain[0]['virtual-size'],
                'profile overlay changes the CI installer contents')
        if start < size:
            require(profile.read_tail(path, start, size - start)
                    == profile.read_tail(base, start, size - start),
                    'profile tail changes bytes inside the CI installer')
    return {'base_sha256': expected_base_hash, 'profile_sha256': expected_profile_hash}


def canonical_uuid(value):
    try:
        return type(value) is str and str(uuid.UUID(value)) == value
    except (ValueError, AttributeError):
        return False


def checked_proof(path, board_ci=None):
    doc = json.loads(ci.file_bytes(path, 65536))
    require(type(doc) is dict and type(doc.get('guest_proof')) is dict
            and type(doc.get('source_check')) is dict, 'incomplete development VM proof')
    guest, source = doc['guest_proof'], doc['source_check']
    require(guest.get('architecture') == 'arm64'
            and ci.SHA.fullmatch(guest.get('booted_image_digest', ''))
            and ci.SHA.fullmatch(guest.get('installed_image_digest', ''))
            and COMPOSEFS.fullmatch(guest.get('composefs_digest', ''))
            and guest.get('installed_composefs_digest') == guest['composefs_digest']
            and COMPOSEFS.fullmatch(guest.get('booted_composefs_digest', ''))
            and doc.get('installer_composefs') == guest['booted_composefs_digest']
            and HEX.fullmatch(guest.get('target_uki_sha256', ''))
            and HEX.fullmatch(doc.get('installer_medium_sha256', '')),
            'development VM image, composefs or UKI identity is invalid')
    ids = guest.get('partuuids')
    require(type(ids) is dict and set(ids) == {'efi', 'boot', 'root'}
            and all(canonical_uuid(value) for value in ids.values())
            and len(set(ids.values())) == 3 and canonical_uuid(guest.get('disk_guid'))
            and source.get('schema') == 'tilo.windows-direct-vm-template-check.v1'
            and source.get('preserved_gpt_verified') is True
            and source.get('preserved_edge_samples_verified') is True
            and source.get('final_partition_count') == 3
            and source.get('disk_guid') == guest['disk_guid']
            and source.get('final_partuuids') == ids,
            'development VM proof differs from its independently checked source GPT')
    if board_ci is None:
        require(doc.get('schema') == 'tilo.windows-vm-install-proof-check.v3'
                and guest.get('schema') == 'tilo.vm-install-image-proof.v3'
                and doc.get('first_boot_verified') is True
                and guest['booted_image_digest'] != guest['installed_image_digest']
                and guest.get('copied_manifest_digest') == guest['installed_image_digest']
                and guest['booted_composefs_digest'] == guest['composefs_digest']
                and guest.get('selected_target') is None,
                'generic development evidence must prove complete self-copy and first boot')
    else:
        selected = guest.get('selected_target', {})
        declared = selected.get('provenance', {})
        require(doc.get('schema') == 'tilo.windows-vm-install-proof-check.v4'
                and guest.get('schema') == 'tilo.vm-install-image-proof.v4'
                and type(doc.get('first_boot_verified')) is bool
                and guest.get('copied_manifest_digest') is None
                and selected.get('schema') == 'tilo.installer-dev-target.v1'
                and selected.get('architecture') == 'aarch64'
                and selected.get('hardware_profile') == 'lenovo-83ed'
                and selected.get('target_ref') == board_ci['image']
                and selected.get('target_digest') == board_ci['image'].split('@')[1]
                and guest['installed_image_digest'] == selected['target_digest']
                and selected.get('update_origin') == board_ci['update_origin']
                and guest.get('installed_update_origin')
                == 'ostree-unverified-image:docker://' + board_ci['update_origin']
                and selected.get('verifier_key_sha256') is None
                and declared.get('source_revision') == board_ci['source_revision']
                and declared.get('invocation_id') == board_ci['invocation_id']
                and all(declared.get(field) is None for field in
                        ('generation', 'firmware_candidate', 'firmware_manifest_sha256')),
                'development Lenovo proof differs from live CI image authority')
    return doc


def add_arguments(parser):
    parser.add_argument('--development', action='store_true',
                        help='explicit CI-built development image admission; no release signature claim')
    for name in ('dev-ci-run-id', 'dev-generic-ci-run-id',
                 'dev-board-medium-run-id', 'dev-generic-medium-run-id'):
        parser.add_argument('--' + name)
    for name in ('dev-ci-config', 'dev-generic-ci-config',
                 'dev-board-medium', 'dev-generic-medium', 'dev-setup-handoff'):
        parser.add_argument('--' + name, type=Path)


def verify(args):
    fields = ('dev_ci_run_id', 'dev_generic_ci_run_id', 'dev_board_medium_run_id',
              'dev_generic_medium_run_id', 'dev_ci_config', 'dev_generic_ci_config',
              'dev_board_medium', 'dev_generic_medium', 'dev_setup_handoff', 'generic_ci_image',
              'generic_child_manifest', 'generic_vm_proof_checked')
    require(all(getattr(args, field, None) is not None for field in fields),
            'development physical admission requires paired image, medium and VM evidence')
    require(getattr(args, 'verify_key', None) is None,
            'development admission must not claim a release verification key')
    board_ci = ci.verify_image(SimpleNamespace(image=args.ci_image, profile='lenovo-83ed',
        ci_run_id=args.dev_ci_run_id, manifest=args.ci_child_manifest, config=args.dev_ci_config))
    generic_ci = ci.verify_image(SimpleNamespace(image=args.generic_ci_image, profile='generic',
        ci_run_id=args.dev_generic_ci_run_id, manifest=args.generic_child_manifest,
        config=args.dev_generic_ci_config))
    require(board_ci['source_revision'] == generic_ci['source_revision']
            and board_ci['invocation_id'] == generic_ci['invocation_id'],
            'development board and generic images are not one CI cohort')
    board = checked_proof(args.vm_proof_checked, board_ci)
    generic = checked_proof(args.generic_vm_proof_checked)
    board_medium = media.verify_medium(int(args.dev_board_medium_run_id))
    generic_medium = media.verify_medium(int(args.dev_generic_medium_run_id))
    for medium in (board_medium, generic_medium):
        require(medium['source'].get('base_image') == generic_ci['image']
                and medium['source'].get('base_source') == generic_ci['source_revision'],
                'CI installer base differs from the verified development cohort')
    require(board_medium['assembly_source'] == generic_medium['assembly_source']
            and board_medium['source'].get('target_image') == board_ci['image']
            and board_medium['source'].get('target_ci_run_id') == board_ci['invocation_id']
            and board_medium['source'].get('target_update_origin') == board_ci['update_origin']
            and not generic_medium['source'].get('target_image'),
            'development installer pair has inconsistent selected target or assembly identities')
    board_profile = checked_profile(args.dev_board_medium,
        board_medium['tilo-dev-installer-aarch64.qcow2.sha256'], board['installer_medium_sha256'],
        args.dev_setup_handoff)
    generic_profile = checked_profile(args.dev_generic_medium,
        generic_medium['tilo-dev-installer-aarch64.qcow2.sha256'], generic['installer_medium_sha256'],
        args.dev_setup_handoff)
    evidence = {'schema': 'tilo.windows-direct-ci-dev-admission.v1',
                'board_ci': board_ci, 'generic_ci': generic_ci,
                'board_medium': board_medium, 'generic_medium': generic_medium,
                'board_profile': board_profile, 'generic_profile': generic_profile,
                'generic_vm_proof_sha256': file_hash(args.generic_vm_proof_checked)}
    provenance = {**board_ci, 'key_sha256': None, 'generation': None,
                  'firmware_candidate': None, 'firmware_manifest_sha256': None,
                  'statement_sha256': None}
    return provenance, board, {'development_admission': evidence}
