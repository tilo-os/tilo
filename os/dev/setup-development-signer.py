#!/usr/bin/env python3
"""Provision a new rootful development EFI/PCR signer and public inputs.

Never imports production keys, overwrites an identity, or enrolls firmware.
Run from a reviewed checkout as root; use an independently selected directory
outside checkout, runner temporary directories and artifact upload roots.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil

spec = importlib.util.spec_from_file_location('development_signer', Path(__file__).with_name('development-signer.py'))
signer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signer)


def provision(directory):
    if os.getuid() != 0:
        raise ValueError('provisioning requires root')
    directory = Path(directory)
    if not directory.is_absolute() or directory != Path(os.path.normpath(directory)):
        raise ValueError('signer directory must be an absolute canonical path')
    checkout = Path(__file__).resolve().parents[2]
    forbidden = [checkout]
    for name in ('RUNNER_TEMP', 'GITHUB_WORKSPACE'):
        if os.environ.get(name):
            forbidden.append(Path(os.environ[name]).resolve())
    if any(directory == root or root in directory.parents for root in forbidden):
        raise ValueError('private signer must be outside checkout and runner temporary paths')
    signer.trusted(directory.parent, directory=True)
    crypto = signer.trusted(Path('/usr/local/bin/tilo-crypto'))
    previous_umask = os.umask(0o077)
    try:
        directory.mkdir(mode=0o700, exist_ok=False)
        # Leave an incomplete identity visibly incomplete if provisioning fails;
        # never silently regenerate keys at an already selected path.
        signer.run([str(crypto), 'generate-tpm-authority-key',
                    str(directory / 'pcr-signing.key'),
                    str(directory / 'tpm2-pcr-signing.public.pem')])
        signer.run([str(crypto), 'generate-secure-boot-cert', 'Tilo Development Test Signer',
                    str(directory / 'efi-signing.key'), str(directory / 'efi-signing.crt')])
        destination = directory / 'signer'
        shutil.copyfile(Path(__file__).with_name('development-signer.py'), destination)
        destination.chmod(0o700)
        (directory / 'purpose').write_bytes(signer.PURPOSE)
        certificate_sha256 = signer.run([str(crypto), 'certificate-sha256',
                                         str(directory / 'efi-signing.crt')]).decode().strip()
        return {'purpose': 'tilo-development', 'signer': str(destination),
                'efi_certificate': str(directory / 'efi-signing.crt'),
                'efi_certificate_sha256': certificate_sha256,
                'pcr_public': str(directory / 'tpm2-pcr-signing.public.pem')}
    finally:
        os.umask(previous_umask)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(provision(args.directory), indent=2, sort_keys=True))
    except (OSError, ValueError) as error:
        parser.exit(1, f'development signer provisioning: {error}\n')


if __name__ == '__main__':
    main()
