#!/usr/bin/env python3
"""Real TLS/Cosign retained-rootfs linkage, using an explicitly synthetic rootfs."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import urllib.request
from unittest.mock import patch

from tls_registry_fixture import tls_registry, run

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('retention', DEV / 'retain-development-rootfs.py')
retention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retention)


def rejects(call, label):
    try:
        call()
    except ValueError:
        return
    raise AssertionError(f'accepted {label}')


def main():
    native = platform.machine()
    if os.getuid() != 0 or native not in ('x86_64', 'aarch64'):
        raise SystemExit('requires native disposable root CI container')
    with tls_registry() as authority, tempfile.TemporaryDirectory(prefix='tilo-retention-fixture-', dir='/var/lib') as temporary:
        root = Path(temporary)
        auth = root / 'auth.json'
        auth.write_text('{"auths":{}}')
        auth.chmod(0o600)
        environment = {name: value for name, value in os.environ.items()
                       if not name.startswith(('COSIGN_', 'CONTAINER_', 'DOCKER_', 'REGISTRY_'))}
        environment.pop('SSL_CERT_FILE', None)
        environment.pop('SSL_CERT_DIR', None)
        environment.update(COSIGN_PASSWORD='', REGISTRY_AUTH_FILE=str(auth))
        image_id = None
        with patch.dict(os.environ, environment, clear=True):
            try:
                key = root / 'cosign'
                run('cosign', 'generate-key-pair', '--output-key-prefix', str(key))
                Path(str(key) + '.key').chmod(0o600)
                signer = json.loads(run('python3', str(DEV / 'setup-development-signer.py'), str(root / 'signer')))
                (root / 'owner.pub').write_text('ab' * 32 + '\n')
                public = root / 'public'
                manifest = json.loads(run('python3', str(DEV / 'development-trust.py'), 'create',
                    '--cosign-public', str(key) + '.pub', '--pcr-public', signer['pcr_public'],
                    '--efi-certificate', signer['efi_certificate'], '--registry-owner', str(root / 'owner.pub'),
                    '--output', str(public)))['manifest_sha256']
                context = root / 'context'
                image_root = context / 'rootfs'
                target = 'code.podesta.ai/qos/tilo-development:retention-fixture-' + native
                revision, generation, epoch = 'b' * 40, 7, 1700000000
                run('python3', str(DEV / 'install-public-trust.py'), '--source', str(public), '--official', str(public),
                    '--destination', str(image_root / 'usr/share/tilo'), '--mode', 'development',
                    '--manifest-sha256', manifest, '--registry-owner', 'ab' * 32,
                    '--target-ref', target, '--image-root', str(image_root))
                generation_path = image_root / 'usr/lib/tilo/generation'
                generation_path.parent.mkdir(parents=True, exist_ok=True)
                generation_path.write_text('7\n')
                modules = image_root / 'usr/lib/modules/fixture-1'
                modules.mkdir(parents=True)
                (modules / 'vmlinuz').write_bytes(b'synthetic, non-executable kernel fixture\n')
                (modules / 'initramfs.img').write_bytes(b'synthetic, non-bootable initramfs fixture\n')
                (context / 'Containerfile').write_text('FROM scratch\nCOPY --chown=0:0 rootfs/ /\n'
                    f'LABEL org.opencontainers.image.revision={revision}\n'
                    'ENTRYPOINT ["/image-code-must-never-execute"]\n')
                run('podman', '--remote=false', 'build', '--network=none', '--pull=never',
                    '--iidfile', str(root / 'iid'), str(context))
                image_id = 'sha256:' + (root / 'iid').read_text().strip().removeprefix('sha256:')
                handoff = {'schema': 1, 'purpose': 'tilo-development-raw-rootfs-handoff',
                    'revision': revision, 'architecture': native, 'public_manifest_sha256': manifest,
                    'target_update_ref': target, 'generation': generation, 'source_date_epoch': epoch,
                    'raw_rootfs_id': image_id, 'installable': False, 'sealed': False, 'published': False,
                    'validation': 'captured build inputs only; independently revalidate before reuse'}
                handoff_path = root / 'handoff.json'
                handoff_path.write_text(json.dumps(handoff))
                args = SimpleNamespace(receipt=handoff_path, private_key=Path(str(key) + '.key'), public_bundle=public,
                    revision=revision, architecture=native, manifest_sha256=manifest, target=target,
                    generation=generation, source_date_epoch=epoch)
                repository = authority + '/tilo-development-rootfs'
                # Only redirect the publication destination. All actual Podman,
                # TLS, mounted identity and Cosign operations remain unchanged.
                with patch.object(retention, 'REPOSITORY', repository):
                    receipt = retention.retain(args)
                    rejects(lambda: retention.retain(SimpleNamespace(**(vars(args) | {'source_date_epoch': epoch + 1}))),
                            'different independently selected epoch')
                reference = receipt['image']
                digest = reference.split('@')[1]

                def registry_bytes(path, accept):
                    request = urllib.request.Request(f'https://{authority}/v2/tilo-development-rootfs/{path}',
                        headers={'Accept': accept})
                    with urllib.request.urlopen(request, timeout=10) as response:
                        return response.read()

                manifest_bytes = registry_bytes('manifests/' + digest, 'application/vnd.oci.image.manifest.v1+json')
                assert 'sha256:' + hashlib.sha256(manifest_bytes).hexdigest() == digest
                remote_manifest = json.loads(manifest_bytes)
                assert remote_manifest['config']['digest'] == image_id
                config_bytes = registry_bytes('blobs/' + image_id, 'application/octet-stream')
                assert 'sha256:' + hashlib.sha256(config_bytes).hexdigest() == image_id
                config = json.loads(config_bytes)
                assert config['architecture'] == {'x86_64': 'amd64', 'aarch64': 'arm64'}[native]
                assert config['config']['Labels']['org.opencontainers.image.revision'] == revision
                verified = run('cosign', 'verify', '--insecure-ignore-tlog=true', '--key', str(public / 'cosign.pub'), reference)
                expected_handoff = retention.receipt_digest(handoff)
                retention.verify_signature(verified, reference, expected_handoff)
                assert receipt['handoff_sha256'] == expected_handoff and receipt['handoff'] == handoff
                rejects(lambda: retention.verify_signature(verified, reference, '0' * 64), 'different receipt annotation')
                rejects(lambda: retention.verify_signature(verified, repository + '@sha256:' + '0' * 64,
                                                           expected_handoff), 'different manifest identity')
                print(json.dumps({'result': 'pass', 'architecture': native,
                    'proof': 'TLS registry manifest/config and signed handoff linkage only',
                    'manifest_digest': digest, 'config_digest': image_id, 'handoff_sha256': expected_handoff,
                    'tls_verification_disabled': False, 'synthetic_rootfs': True,
                    'image_code_executed': False, 'boot_proven': False, 'installable': False,
                    'checks': ['independent epoch', 'manifest content hash', 'config ID matches original image',
                               'native architecture and revision', 'real Cosign receipt annotation',
                               'wrong annotation rejected', 'wrong manifest rejected']}))
            finally:
                if image_id:
                    subprocess.run(['podman', '--remote=false', 'rmi', '--force', image_id],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
