#!/usr/bin/env python3
"""Actual native stub/systemd prediction proof, with nonbootable test payloads.

Only disposable CI: keys and synthetic UKIs stay in a private temporary tree.
No firmware, TPM, physical disk, image execution or kernel boot is involved.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import sys
import tempfile

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
import development_prediction as prediction


def run(*argv, env=None):
    result = subprocess.run(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        # Never reproduce private signer/key-generation output.
        raise RuntimeError(f'{Path(argv[0]).name} failed with status {result.returncode}')
    return result.stdout


def rejects(call, label):
    try:
        call()
    except ValueError:
        return
    raise AssertionError(f'verifier accepted {label}')


def main():
    native = platform.machine()
    if os.getuid() != 0 or native not in ('x86_64', 'aarch64'):
        raise SystemExit('requires native root Linux in disposable CI')
    suffix = {'x86_64': 'x64', 'aarch64': 'aa64'}[native]
    stub = Path(f'/usr/lib/systemd/boot/efi/linux{suffix}.efi.stub')
    version = run('rpm', '-qf', '--qf', '%{VERSION}-%{RELEASE}', str(stub)).decode()
    tool_version = run('rpm', '-qf', '--qf', '%{VERSION}-%{RELEASE}', prediction.MEASURE).decode()
    if version != tool_version:
        raise RuntimeError('fixture stub and trusted measurement package versions differ')
    with tempfile.TemporaryDirectory(prefix='tilo-prediction-fixture-', dir='/var/lib') as temporary:
        root = Path(temporary)
        signer = json.loads(run('python3', str(DEV / 'setup-development-signer.py'), str(root / 'signer')))
        environment = {'PATH': os.defpath, 'LANG': 'C.UTF-8', 'COSIGN_PASSWORD': ''}
        run('cosign', 'generate-key-pair', '--output-key-prefix', str(root / 'cosign'), env=environment)
        (root / 'owner.pub').write_text('ab' * 32 + '\n')

        def bundle(destination, pcr_key):
            return json.loads(run('python3', str(DEV / 'development-trust.py'), 'create',
                '--cosign-public', str(root / 'cosign.pub'), '--pcr-public', str(pcr_key),
                '--efi-certificate', signer['efi_certificate'], '--registry-owner', str(root / 'owner.pub'),
                '--output', str(destination)))['manifest_sha256']

        public = root / 'public'
        manifest = bundle(public, signer['pcr_public'])
        image = root / 'image'
        live = image / 'boot/EFI/Linux'
        live.mkdir(parents=True)
        target = image / prediction.TARGET
        target.parent.mkdir(mode=0o700, parents=True)
        generation_file = image / 'usr/lib/tilo/generation'
        generation_file.parent.mkdir(parents=True)
        generation_file.write_text('7\n')
        seal = 'a' * 128
        payloads = {'linux': b'not an executable kernel: native measurement fixture\n',
                    'initrd': b'not a bootable initramfs\n',
                    'cmdline': f'tilo.generation=7 composefs={seal}'.encode() + b'\0'}
        if native == 'aarch64':
            payloads['dtb'] = b'explicit static device-tree measurement fixture\n'
        # Keep real stub geometry; objcopy adds ordinary allocated PE sections.
        data = stub.read_bytes()
        header = struct.unpack_from('<I', data, 0x3c)[0]
        image_base = struct.unpack_from('<Q', data, header + 24 + 24)[0]
        image_size = struct.unpack_from('<I', data, header + 24 + 56)[0]
        next_address = (image_size + 4095) & ~4095
        argv = ['/usr/bin/objcopy']
        for name, value in payloads.items():
            path = root / name
            path.write_bytes(value)
            argv.extend(['--add-section', f'.{name}={path}', '--change-section-vma',
                         f'.{name}={image_base + next_address:#x}', '--set-section-flags',
                         f'.{name}=alloc,load,readonly,data'])
            next_address += 4096
        unsigned = root / 'unsigned.efi'
        run(*argv, str(stub), str(unsigned))
        run(signer['signer'], 'sign-efi', str(unsigned), str(target))
        shutil.copyfile(target, live / 'fixture.efi')
        # Independently derive producer input from original payload files and
        # objcopy's original stub SBAT, not the verifier's section extractor.
        run('/usr/bin/objcopy', '--dump-section', f'.sbat={root / "sbat"}', str(stub), str(root / 'ignored.stub'))
        measure = [prediction.MEASURE, 'policy-digest', '--json=short', '--bank=sha256', '--phase=enter-initrd']
        measure.extend(f'--{name}={root / name}' for name in (*payloads, 'sbat'))
        policy = prediction.measured_policy(run(*measure))
        sidecar = target.parent / 'predictions/generation-7.json'
        sidecar.parent.mkdir(mode=0o700)

        def signed_document(approved):
            message, signature = root / 'message', root / 'signature'
            message.write_bytes(approved + struct.pack('>Q', 7))
            run(signer['signer'], 'sign-sha256-rsa-pkcs1', str(message), str(signature))
            sidecar.write_text(json.dumps({'generation': 7, 'pcr_bank': 'sha256', 'pcrs': [11],
                'approved_policy': base64.b64encode(approved).decode(),
                'signature': base64.b64encode(signature.read_bytes()).decode()}))
            signature.unlink()  # Signer creates outputs exclusively.

        signed_document(policy)
        detached = Path(str(sidecar) + '.uki.sig')
        run(signer['signer'], 'sign-sha256-rsa-pkcs1', str(target), str(detached))

        def verify(**changes):
            arguments = dict(root=image, public_bundle=public, manifest_sha256=manifest,
                             architecture=native, generation=7, composefs_digest=seal, systemd_version=version)
            return prediction.verify_prediction(**(arguments | changes))

        receipt = verify()
        rejects(lambda: verify(generation=8), 'wrong generation')
        rejects(lambda: verify(systemd_version='259.0-fixture-mismatch'), 'wrong host/stub version')
        original_document = sidecar.read_bytes()
        signed_document(b'\x42' * 32)
        rejects(verify, 'correctly signed policy for different PCR state')
        sidecar.write_bytes(original_document)
        original_target = target.read_bytes()
        altered = bytearray(original_target)
        altered[2] ^= 1
        target.write_bytes(altered)
        rejects(verify, 'tampered authenticated target bytes')
        target.write_bytes(original_target)
        original_detached = detached.read_bytes()
        detached.write_bytes(b'\0' * len(original_detached))
        rejects(verify, 'invalid exact-target detached signature')
        detached.write_bytes(original_detached)
        other = json.loads(run('python3', str(DEV / 'setup-development-signer.py'), str(root / 'other')))
        wrong_public = root / 'wrong-public'
        wrong_manifest = bundle(wrong_public, other['pcr_public'])
        rejects(lambda: verify(public_bundle=wrong_public, manifest_sha256=wrong_manifest), 'independently pinned wrong PCR key')
        print(json.dumps({'result': 'pass', 'proof': 'offline native systemd policy and signatures only',
            'architecture': native, 'systemd_version': version, 'stub_sha256': hashlib.sha256(data).hexdigest(),
            'package_versions': run('rpm', '-qf', '--qf', '%{NEVRA}\n', str(stub), prediction.MEASURE).decode().splitlines(),
            'prediction': receipt, 'bootable_kernel_payload': False,
            'rejected': ['wrong generation', 'wrong systemd version', 'signed wrong policy',
                         'tampered UKI', 'invalid detached UKI signature', 'wrong independently pinned PCR key']}))


if __name__ == '__main__':
    main()
