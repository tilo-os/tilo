#!/usr/bin/env python3
"""Exercise isolated development signing with real native EFI and PCR signatures.

Run only in the disposable development-source CI container, as root. All test
keys live in a new private /var/lib directory and are removed with the fixture.
"""
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import tempfile

DEV = Path(__file__).resolve().parents[1] / 'dev'


def run(*args, success=True, env=None):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    if (result.returncode == 0) != success:
        detail = result.stderr.decode(errors='replace')[-4096:]
        raise RuntimeError(f'{args[0]} {args[1]} unexpected exit {result.returncode}: {detail}')
    return result.stdout


def bootloader_selection(root, original, signed, certificate, metadata, machine):
    """Prove native bootctl's tmpfs copy, not FAT, firmware or OS bootability."""
    architecture = platform.machine()
    target = root / 'bootctl-root'
    source = target / 'usr/lib/systemd/boot/efi'
    source.mkdir(parents=True)
    (target / 'etc').mkdir()
    (target / 'etc/os-release').write_text('ID=tilo-fixture\nPRETTY_NAME="Tilo fixture"\n')
    (target / 'boot').mkdir()
    unsigned = source / original.name
    companion = source / (original.name + '.signed')
    shutil.copyfile(original, unsigned)
    shutil.copyfile(signed, companion)
    assert unsigned.read_bytes() != companion.read_bytes()

    def verify(path, arch=architecture, success=True):
        output = run('python3', str(DEV / 'verify-development-loader.py'),
                     '--original', str(unsigned), '--signed', str(path),
                     '--certificate', str(certificate),
                     '--certificate-sha256', metadata['efi_certificate_sha256'],
                     '--architecture', arch, success=success)
        return json.loads(output) if success else None

    receipt = verify(companion)
    verify(source / 'missing.efi.signed', success=False)
    verify(unsigned, success=False)
    # A bounded certificate table is not necessarily an actual signature.
    # Exercise real sbverify on empty and unrecognized WIN_CERTIFICATE records.
    for revision, kind in ((0, 0), (0x0200, 2)):
        fake_bytes = bytearray(unsigned.read_bytes())
        pe = struct.unpack_from('<I', fake_bytes, 0x3C)[0]
        security = pe + 24 + 112 + 8 * 4
        assert struct.unpack_from('<II', fake_bytes, security) == (0, 0)
        fake_bytes.extend(b'\0' * (-len(fake_bytes) % 8))
        offset = len(fake_bytes)
        fake_bytes.extend(struct.pack('<IHH', 8, revision, kind))
        struct.pack_into('<II', fake_bytes, security, offset, 8)
        fake = root / f'empty-certificate-{revision}-{kind}.efi'
        fake.write_bytes(fake_bytes)
        verify(fake, success=False)
    verify(companion, arch={'x86_64': 'aarch64', 'aarch64': 'x86_64'}[architecture], success=False)
    tampered = root / 'tampered-loader.efi'
    payload = bytearray(signed.read_bytes())
    payload[2] ^= 1  # Authenticated DOS header byte; preserve MZ and PE structure.
    tampered.write_bytes(payload)
    verify(tampered, success=False)
    other = json.loads(run('python3', str(DEV / 'setup-development-signer.py'),
                          str(root / 'other-identity')))
    wrong_key = root / 'wrong-key-loader.efi'
    run(other['signer'], 'sign-efi', str(original), str(wrong_key))
    verify(wrong_key, success=False)

    # Never fall back to the host loader, write EFI variables, or open a disk.
    # Relaxation skips FAT and partition checks, but systemd still requires
    # STATX_ATTR_MOUNT_ROOT. A private tmpfs supplies a real filesystem root.
    environment = {'PATH': os.defpath, 'LANG': 'C.UTF-8',
                   'SYSTEMD_RELAX_ESP_CHECKS': '1',
                   'KERNEL_INSTALL_CONF_ROOT': str(target / 'etc/kernel')}
    destinations = [target / 'boot/EFI/systemd' / original.name,
                    target / 'boot/EFI/BOOT' / f'BOOT{machine.upper()}.EFI']
    esp = target / 'boot'
    run('/usr/bin/mount', '-t', 'tmpfs', '-o', 'size=32M,mode=0700,nodev,nosuid,noexec',
        'tilo-fixture-esp', str(esp))
    try:
        try:
            run('/usr/bin/bootctl', 'install', '--root', str(target), '--esp-path=/boot',
                '--install-source=image', '--no-variables', '--random-seed=no',
                '--entry-token=literal:tilo-fixture', env=environment)
        except RuntimeError as error:
            raise RuntimeError(f'bootctl install into disposable tmpfs ESP failed: {error}') from error
        for destination in destinations:
            assert destination.is_file() and not destination.is_symlink()
            assert destination.read_bytes() == signed.read_bytes()
            verify(destination)
        assert not (esp / 'loader/random-seed').exists()
    finally:
        run('/usr/bin/umount', str(esp))
    print(json.dumps({'result': 'pass', 'proof': 'native bootctl tmpfs copy only',
                      'architecture': architecture,
                      'bootctl_version': run('/usr/bin/bootctl', '--version').decode().splitlines()[0],
                      'rpm_nevras': run('rpm', '-qf', '--qf', '%{NEVRA}\n',
                                        '/usr/bin/bootctl', str(original)).decode().splitlines(),
                      'loader_verification': receipt,
                      'esp_paths': [str(path.relative_to(target / 'boot')) for path in destinations],
                      'rejected': ['missing companion', 'unsigned loader', 'empty certificate table', 'wrong architecture',
                                   'tampered loader', 'wrong signing key']}))


def main():
    if os.getuid() != 0:
        raise SystemExit('disposable signer fixture requires root')
    # The already privileged CI container supplies CAP_SYS_ADMIN. Isolate mount
    # changes again and disable propagation before creating the temporary ESP.
    os.unshare(os.CLONE_NEWNS)
    run('/usr/bin/mount', '--make-rprivate', '/')
    with tempfile.TemporaryDirectory(prefix='tilo-signer-fixture-', dir='/var/lib') as directory:
        root = Path(directory)
        home = root / 'identity'
        provision = ['python3', str(DEV / 'setup-development-signer.py'), str(home)]
        metadata = json.loads(run(*provision))
        signer = metadata['signer']
        certificate = root / 'exported.crt'
        run(signer, 'export-efi-cert', str(certificate))
        identity = run('/usr/local/bin/tilo-crypto', 'certificate-sha256',
                       str(certificate)).decode().strip()
        assert identity == metadata['efi_certificate_sha256']
        assert (home / 'efi-signing.key').stat().st_mode & 0o077 == 0
        assert (home / 'pcr-signing.key').stat().st_mode & 0o077 == 0
        run(*provision, success=False)
        assert certificate.read_bytes() == (home / 'efi-signing.crt').read_bytes()
        message = root / 'message'
        message.write_bytes(b'generation-bound PCR authorization fixture\0' + (42).to_bytes(8, 'big'))
        signature = root / 'pcr.signature'
        run(signer, 'sign-sha256-rsa-pkcs1', str(message), str(signature))
        verification = ['/usr/local/bin/tilo-crypto', 'verify-rsa-sha256',
                        metadata['pcr_public'], str(message), str(signature)]
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
        bootloader_selection(root, original, signed, certificate, metadata, machine)
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
