"""Offline PCR11 prediction verification; no TPM, image execution or boot proof.

The caller supplies a pinned public bundle, generation, composefs seal and exact
systemd package version independently of the mounted image. Only the explicit
target UKI is accepted. Measurement semantics follow systemd v259:
https://github.com/systemd/systemd/blob/v259/src/measure/measure-tool.c
https://github.com/systemd/systemd/blob/v259/src/boot/stub.c
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import struct
import tempfile

import development_boot as boot

MEASURE = '/usr/lib/systemd/systemd-measure'
TARGET = Path('boot/tilo-target/tilo-target.efi')
MEASURED = ('linux', 'osrel', 'cmdline', 'initrd', 'ucode', 'splash', 'dtb',
            'uname', 'sbat', 'pcrpkey')
UNMEASURED = {'.text', '.rodata', '.data', '.bss', '.sdmagic', '.reloc', '.pcrsig'}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate prediction JSON member')
        result[key] = value
    return result


def json_document(raw):
    return json.loads(raw, object_pairs_hook=unique_object)


def prediction_document(raw, generation):
    document = json_document(raw)
    if (not isinstance(document, dict) or type(document.get('generation')) is not int
            or document['generation'] != generation or document.get('pcr_bank') != 'sha256'
            or document.get('pcrs') != [11]):
        raise ValueError('prediction must bind the exact generation and SHA256 PCR11')
    values = []
    for field in ('approved_policy', 'signature'):
        value = document.get(field)
        if not isinstance(value, str):
            raise ValueError('prediction requires base64 policy and signature')
        try:
            decoded = base64.b64decode(value, validate=True)
        except ValueError:
            raise ValueError('invalid prediction base64') from None
        if base64.b64encode(decoded).decode() != value:
            raise ValueError('noncanonical prediction base64')
        values.append(decoded)
    if len(values[0]) != 32 or not 256 <= len(values[1]) <= 1024:
        raise ValueError('invalid SHA256 policy or RSA signature size')
    return tuple(values)


def measured_policy(raw):
    document = json_document(raw)
    if not isinstance(document, dict) or set(document) != {'sha256'}:
        raise ValueError('host measurement must return only SHA256')
    rows = document['sha256']
    if (not isinstance(rows, list) or len(rows) != 1 or not isinstance(rows[0], dict)
            or rows[0].get('pcrs') != [11]
            or not isinstance(rows[0].get('pol'), str)
            or not re.fullmatch(r'[0-9a-f]{64}', rows[0]['pol'])):
        raise ValueError('host measurement returned an ambiguous PCR11 policy')
    return bytes.fromhex(rows[0]['pol'])


def measurement_sections(data, architecture):
    sections = boot.uki_sections(data, architecture)
    # Automatic DT selection, profiles and firmware payloads need a separate
    # hardware-aware contract. Do not silently predict the default selection.
    if set(sections) - UNMEASURED - {'.' + name for name in MEASURED}:
        raise ValueError('target has unsupported or hardware-selected UKI sections')
    header = boot.pe.u32(data, 0x3c)
    table = header + 24 + boot.pe.u16(data, header + 20)
    for index in range(boot.pe.u16(data, header + 6)):
        offset = table + index * 40
        name = data[offset:offset + 8].rstrip(b'\0').decode('ascii')
        if name[1:] in MEASURED:
            virtual, raw = boot.pe.u32(data, offset + 8), boot.pe.u32(data, offset + 16)
            if not 0 < virtual <= raw:
                raise ValueError('measured UKI section has incomplete file-backed bytes')
    return {name: sections['.' + name] for name in MEASURED if '.' + name in sections}


def stub_metadata(data):
    """Read the signed .sdmagic raw section, not an arbitrary payload marker."""
    header = boot.pe.u32(data, 0x3c)
    table = header + 24 + boot.pe.u16(data, header + 20)
    for index in range(boot.pe.u16(data, header + 6)):
        offset = table + index * 40
        if data[offset:offset + 8] == b'.sdmagic':
            size, raw = boot.pe.u32(data, offset + 16), boot.pe.u32(data, offset + 20)
            return data[raw:raw + size]
    raise ValueError('signed target UKI is missing systemd stub metadata')


def check_systemd_version(data, version, host_output):
    if not isinstance(version, str) or not re.fullmatch(r'259(?:[.][0-9]+)?(?:[-+][A-Za-z0-9_.+-]+)?', version):
        raise ValueError('an independently pinned systemd 259 package version is required')
    markers = re.findall(rb'#### LoaderInfo: systemd-stub ([^\x00\r\n ]+) ####', data)
    if markers != [version.encode()]:
        raise ValueError('signed target stub differs from pinned systemd version')
    lines = host_output.decode('utf-8', 'strict').splitlines()
    if not lines or lines[0] != f'systemd 259 ({version})':
        raise ValueError('trusted host systemd-measure differs from pinned stub version')


def verify_prediction(root, public_bundle, manifest_sha256, architecture, generation,
                      composefs_digest, systemd_version):
    """Verify a mounted immutable source; caller must hold its mount lifetime.

    The public bundle and systemd package version are caller trust inputs, never
    discovered from the image. Trusted host tool installation is a prerequisite.
    """
    generation = boot.expected_inputs(architecture, generation, composefs_digest)
    root, public_bundle = Path(root), Path(public_bundle)
    # This independently checks the EFI signature and command-line bindings.
    boot_receipt = boot.validate_mounted_boot(root, public_bundle, manifest_sha256,
                                             architecture, generation, composefs_digest)
    targets = [row for row in boot_receipt['ukis'] if row['path'] == TARGET.as_posix()]
    if len(targets) != 1:
        raise ValueError('an explicit signed target UKI is required')
    metadata = boot.trust.validate(public_bundle, manifest_sha256)
    with tempfile.TemporaryDirectory(prefix='tilo-prediction-validation-') as temporary:
        scratch = Path(temporary)
        target = scratch / 'target.efi'
        data = boot.loader.snapshot(boot.hardware.trusted_path(root, TARGET, regular=True), target, boot.UKI_LIMIT)
        digest = hashlib.sha256(data).hexdigest()
        if digest != targets[0]['sha256']:
            raise ValueError('target UKI changed after EFI verification')
        key = scratch / 'pcr-public.pem'
        boot.loader.snapshot(public_bundle / 'tpm2-pcr-signing.public.pem', key, 65536)
        key_info = boot.loader.command(
            ['/usr/local/bin/tilo-crypto', 'rsa-public-key-info', str(key)]).decode().split()
        if len(key_info) != 2 or not key_info[0].isdigit() or not re.fullmatch(r'[0-9a-f]{64}', key_info[1]):
            raise ValueError('Rust-native crypto returned invalid PCR public-key metadata')
        key_digest = key_info[1]
        if key_digest != metadata['identities']['tpm2-pcr-signing.public.pem']:
            raise ValueError('PCR public key changed after independent bundle verification')
        relative = Path(f'boot/tilo-target/predictions/generation-{generation}.json')
        prediction = boot.loader.snapshot(boot.hardware.trusted_path(root, relative, regular=True),
                                          scratch / 'prediction.json', 16384)
        policy, signature = prediction_document(prediction, generation)
        (scratch / 'policy-message').write_bytes(policy + struct.pack('>Q', generation))
        (scratch / 'policy-signature').write_bytes(signature)
        detached = scratch / 'uki-signature'
        detached_data = boot.loader.snapshot(boot.hardware.trusted_path(root, Path(str(relative) + '.uki.sig'),
                                                                       regular=True), detached, 1024)
        if not 256 <= len(detached_data) <= 1024:
            raise ValueError('invalid detached target signature size')
        for message, signed in ((scratch / 'policy-message', scratch / 'policy-signature'), (target, detached)):
            boot.loader.command(['/usr/local/bin/tilo-crypto', 'verify-rsa-sha256',
                                 str(key), str(message), str(signed)])
        sections = measurement_sections(data, architecture)
        check_systemd_version(stub_metadata(data), systemd_version, boot.loader.command([MEASURE, '--version']))
        argv = [MEASURE, 'policy-digest', '--json=short', '--bank=sha256', '--phase=enter-initrd']
        for name, contents in sections.items():
            section = scratch / name
            section.write_bytes(contents)
            argv.append(f'--{name}={section}')
        actual = measured_policy(boot.loader.command(argv))
        if actual != policy:
            raise ValueError('signed prediction differs from recomputed target PCR11 policy')
    return {'schema': 1, 'purpose': 'tilo-development-offline-prediction-verification',
            'generation': generation, 'target_uki_sha256': digest, 'pcr_public_key_sha256': key_digest,
            'manifest_sha256': manifest_sha256, 'systemd_version': systemd_version,
            # Preserve already signature-verified live identities for media
            # consumers; target PCR proof below still applies only to TARGET.
            'signed_ukis': boot_receipt['ukis'],
            'pcr_bank': 'sha256', 'pcrs': [11], 'phase': 'enter-initrd', 'approved_policy': actual.hex(),
            'proof': {'offline_policy_recomputed': True, 'live_pcr': False, 'vm_boot': False},
            'installable': False}
