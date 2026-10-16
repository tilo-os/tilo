#!/usr/bin/python3
"""Create one exact authorization from independently authenticated local inputs.

Expected hash/context/seal/version are operator trust inputs, never values copied
from a pending remote request. Output contains public metadata only.
"""
import argparse
import json
from pathlib import Path
import re
import struct
import tempfile
import development_boot as boot
import development_prediction as prediction
from signing_spool import snapshot, digest, write_new, signer


def authorize(identifier, purpose, source, expected_sha256, architecture, prepared_input,
              generation=None, composefs_digest=None, systemd_version=None):
    if not re.fullmatch(r'[a-z0-9-]{1,64}', identifier) or purpose not in ('loader', 'uki', 'pcr-policy', 'detached-uki'):
        raise ValueError('invalid signing authorization purpose')
    if architecture not in boot.loader.MACHINES or not re.fullmatch(r'[0-9a-f]{64}', expected_sha256):
        raise ValueError('independently pinned input identity required')
    data = snapshot(source, 8 * 1024**2 if purpose == 'loader' else boot.UKI_LIMIT)
    if digest(data) != expected_sha256:
        raise ValueError('signing input differs from authenticated artifact')
    with tempfile.TemporaryDirectory(prefix='tilo-signing-approval-', dir='/var/lib') as tmp:
        scratch = Path(tmp)
        exact = scratch / 'input.efi'
        write_new(exact, data)
        boot.pe.normalized_image(str(exact), require_certificate=purpose in ('pcr-policy', 'detached-uki'))
        pe = boot.pe.u32(data, 0x3c)
        if boot.pe.u16(data, pe + 4) != boot.loader.MACHINES[architecture]:
            raise ValueError('EFI architecture differs')
        if purpose == 'loader' and (boot.pe.u16(data, pe + 24) != 0x20b
                                    or boot.pe.u16(data, pe + 24 + 68) != 10):
            raise ValueError('loader must be a native PE32+ EFI application')
        if purpose != 'loader':
            gen = boot.expected_inputs(architecture, generation, composefs_digest)
            sections = boot.uki_sections(data, architecture)
            boot.validate_cmdline(sections['.cmdline'], gen, composefs_digest)
        if purpose == 'pcr-policy':
            measured = prediction.measurement_sections(data, architecture)
            prediction.check_systemd_version(prediction.stub_metadata(data), systemd_version,
                                            signer.run([prediction.MEASURE, '--version']))
            arguments = [prediction.MEASURE, 'policy-digest', '--json=short', '--bank=sha256', '--phase=enter-initrd']
            for name, content in measured.items():
                path = scratch / name
                write_new(path, content)
                arguments.append(f'--{name}={path}')
            data = prediction.measured_policy(signer.run(arguments)) + struct.pack('>Q', gen)
    write_new(Path(prepared_input), data)
    return {'id': identifier, 'purpose': purpose,
            'operation': 'sign-efi' if purpose in ('loader', 'uki') else 'sign-sha256-rsa-pkcs1',
            'input_sha256': digest(data), 'input_bytes': len(data)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('identifier', 'purpose', 'source', 'expected-sha256', 'architecture', 'prepared-input'):
        parser.add_argument('--' + name, required=True)
    for name in ('generation', 'composefs-digest', 'systemd-version'):
        parser.add_argument('--' + name)
    try:
        print(json.dumps(authorize(**vars(parser.parse_args())), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError):
        parser.exit(1, 'signing authorization rejected; no request-controlled diagnostics emitted\n')
