#!/usr/bin/env python3
"""Exercise isolated development signing with real native EFI and PCR signatures.

Run only in the disposable development-source CI container, as root. All test
keys live in a new private /var/lib directory and are removed with the fixture.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile

DEV = Path(__file__).resolve().parents[1] / 'dev'


def run(*args, success=True):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if (result.returncode == 0) != success:
        raise RuntimeError(f'{args[0]} {args[1]} unexpected exit {result.returncode}')
    return result.stdout


def main():
    if os.getuid() != 0:
        raise SystemExit('disposable signer fixture requires root')
    with tempfile.TemporaryDirectory(prefix='tilo-signer-fixture-', dir='/var/lib') as directory:
        root = Path(directory)
        home = root / 'identity'
        provision = ['python3', str(DEV / 'setup-development-signer.py'), str(home)]
        metadata = json.loads(run(*provision))
        signer = metadata['signer']
        certificate = root / 'exported.crt'
        run(signer, 'export-efi-cert', str(certificate))
        der = run('openssl', 'x509', '-in', str(certificate), '-outform', 'DER')
        assert hashlib.sha256(der).hexdigest() == metadata['efi_certificate_sha256']
        assert (home / 'efi-signing.key').stat().st_mode & 0o077 == 0
        assert (home / 'pcr-signing.key').stat().st_mode & 0o077 == 0
        run(*provision, success=False)
        assert certificate.read_bytes() == (home / 'efi-signing.crt').read_bytes()
        message = root / 'message'
        message.write_bytes(b'generation-bound PCR authorization fixture\0' + (42).to_bytes(8, 'big'))
        signature = root / 'pcr.signature'
        run(signer, 'sign-sha256-rsa-pkcs1', str(message), str(signature))
        verification = ['openssl', 'dgst', '-sha256', '-verify', metadata['pcr_public'],
                        '-signature', str(signature), str(message)]
        run(*verification)
        message.write_bytes(b'different PCR authorization')
        run(*verification, success=False)
        before = signature.read_bytes()
        run(signer, 'sign-sha256-rsa-pkcs1', str(message), str(signature), success=False)
        assert signature.read_bytes() == before
        redirected = root / 'redirected'
        redirected.symlink_to(message)
        failed_output = root / 'failed-output'
        run(signer, 'sign-sha256-rsa-pkcs1', str(redirected), str(failed_output), success=False)
        assert not failed_output.exists()
        key = home / 'pcr-signing.key'
        key.chmod(0o644)
        run(signer, 'sign-sha256-rsa-pkcs1', str(message), str(failed_output), success=False)
        assert not failed_output.exists()
        key.chmod(0o600)
        shared = root / 'shared-output'
        shared.mkdir(mode=0o755)
        run(signer, 'export-efi-cert', str(shared / 'output'), success=False)
        assert not (shared / 'output').exists()
        output_link = root / 'output-link'
        output_link.symlink_to(root, target_is_directory=True)
        run(signer, 'export-efi-cert', str(output_link / 'output'), success=False)
        assert not (root / 'output').exists()
        machine = {'aarch64': 'aa64', 'x86_64': 'x64'}[platform.machine()]
        original = Path('/usr/lib/systemd/boot/efi') / f'systemd-boot{machine}.efi'
        signed = root / 'signed.efi'
        run(signer, 'sign-efi', str(original), str(signed))
        listing = run('sbverify', '--list', str(signed)).decode()
        assert 'signature 1' in listing
        run('sbverify', '--cert', str(certificate), str(signed))
        run('python3', str(DEV / 'verify-authenticode-envelope.py'), str(original), str(signed))
        (home / 'purpose').write_text('not a development signer\n')
        run(signer, 'export-efi-cert', str(failed_output), success=False)
        assert not failed_output.exists()
        print(json.dumps({'result': 'pass', 'architecture': platform.machine(),
                          'checks': ['certificate fingerprint', 'PCR signature', 'changed payload rejected',
                                     'native EFI signature and preserved envelope', 'private key permissions',
                                     'exclusive private output', 'redirected output rejected',
                                     'redirected input rejected', 'wrong purpose rejected']}))


if __name__ == '__main__':
    main()
