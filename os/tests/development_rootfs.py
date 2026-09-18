#!/usr/bin/env python3
"""Native disposable proof of the real pre-seal development loader producer.

Builds a minimal Fedora fixture, not Tilo, and proves no boot or composefs seal.
Private keys stay outside every build context. Run only inside native CI.
"""
import json
from datetime import datetime
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

DEV = Path(__file__).resolve().parents[1] / 'dev'
BASE = 'registry.fedoraproject.org/fedora@sha256:61beafd34111e1cb85fb49377ceadeee0a53622dbc20670ed8ca303f0e17ed9c'
REVISION = 'b' * 40
EPOCH = 1700000000
ENVIRONMENT = os.environ.copy()
CRYPTO = Path('/usr/local/bin/tilo-crypto')


def raw_rootfs_containerfile():
    return (f'FROM {BASE}\n'
            'RUN dnf -y install python3 systemd-boot-unsigned && dnf clean all\n'
            'COPY tilo-crypto /usr/local/bin/tilo-crypto\n'
            'COPY rootfs/ /\n'
            f'LABEL org.opencontainers.image.revision={REVISION}\n')


def stage_rootfs_crypto(context, source=CRYPTO):
    if not source.is_file() or not os.access(source, os.X_OK):
        raise RuntimeError(f'native rootfs verifier missing executable {source}')
    destination = context / 'tilo-crypto'
    shutil.copy2(source, destination)
    return destination


def run(*arguments, env=None, error=None):
    result = subprocess.run(arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env=ENVIRONMENT if env is None else env)
    stderr = result.stderr.decode(errors='replace')
    if error is not None:
        if not result.returncode or error not in stderr:
            raise RuntimeError(f'expected rejection {error!r}: {stderr[-4096:]}')
    elif result.returncode:
        raise RuntimeError(f'{arguments[0]} failed: {stderr[-4096:]}')
    return result.stdout.decode()


def podman(*arguments):
    return run('podman', '--remote=false', *arguments)


def main():
    global ENVIRONMENT
    architecture = platform.machine()
    if os.getuid() != 0 or architecture not in ('x86_64', 'aarch64'):
        raise SystemExit('requires disposable rootful native CI')
    images = []
    with tempfile.TemporaryDirectory(prefix='tilo-rootfs-fixture-', dir='/var/lib') as temporary:
        root = Path(temporary)
        storage = root / 'storage.conf'
        storage.write_text('[storage]\ndriver="vfs"\n'
                           f'graphroot={json.dumps(str(root / "graphroot"))}\n'
                           f'runroot={json.dumps(str(root / "runroot"))}\n')
        # Raw build, producer subprocess, inspection and subsequent stage reuse
        # must all use this same explicitly selected local store.
        ENVIRONMENT = os.environ | {'CONTAINERS_STORAGE_CONF': str(storage)}
        try:
            metadata = json.loads(run('python3', str(DEV / 'setup-development-signer.py'), str(root / 'identity')))
            cosign = root / 'cosign'
            run('cosign', 'generate-key-pair', '--output-key-prefix', str(cosign),
                env=ENVIRONMENT | {'COSIGN_PASSWORD': ''})
            owner = root / 'owner.pub'
            owner.write_text('ab' * 32 + '\n')
            bundle = root / 'public'
            public = json.loads(run('python3', str(DEV / 'development-trust.py'), 'create',
                '--cosign-public', str(cosign) + '.pub', '--pcr-public', metadata['pcr_public'],
                '--efi-certificate', metadata['efi_certificate'], '--registry-owner', str(owner),
                '--output', str(bundle)))
            digest = public['manifest_sha256']
            target = f'code.sandbox.example/qos/tilo-development:fixture-{architecture}'
            context = root / 'context'
            context.mkdir()
            stage_rootfs_crypto(context)
            image_root = context / 'rootfs'
            run('python3', str(DEV / 'install-public-trust.py'), '--source', str(bundle),
                '--official', str(bundle), '--destination', str(image_root / 'usr/share/tilo'),
                '--mode', 'development', '--manifest-sha256', digest, '--registry-owner', 'ab' * 32,
                '--target-ref', target, '--image-root', str(image_root))
            (context / 'Containerfile').write_text(raw_rootfs_containerfile())
            iidfile = root / 'raw.iid'
            # Network is confined to the disposable fixture's initial package setup.
            podman('build', '--iidfile', str(iidfile), str(context))
            raw = iidfile.read_text().strip()
            images.append(raw)
            arguments = ['--rootfs-id', raw, '--architecture', architecture, '--revision', REVISION,
                         '--source-date-epoch', str(EPOCH),
                         '--public-bundle', str(bundle), '--manifest-sha256', digest,
                         '--signer', metadata['signer'], '--target', target]

            def produce(overrides=(), error=None, env=None):
                output = run('python3', str(DEV / 'sign-development-rootfs.py'), *arguments,
                             *overrides, error=error, env=env)
                return json.loads(output) if error is None else None

            produce(['--architecture', {'aarch64': 'x86_64', 'x86_64': 'aarch64'}[architecture]],
                    error='requires native architecture')
            produce(['--revision', 'c' * 40], error='identity, architecture or revision differs')
            produce(['--manifest-sha256', '0' * 64], error='manifest')
            produce(error='remote container connections', env=ENVIRONMENT | {'CONTAINER_HOST': 'ssh://unused.invalid'})
            receipt = produce()
            signed = receipt['signed_rootfs_id']
            images.append(signed)
            assert raw != signed and receipt['rootfs_id'] == raw
            assert receipt['revision'] == REVISION and receipt['manifest_sha256'] == digest
            assert receipt['source_date_epoch'] == EPOCH
            created = json.loads(podman('image', 'inspect', signed))[0]['Created']
            assert datetime.fromisoformat(created.replace('Z', '+00:00')).timestamp() == EPOCH, created
            name = 'systemd-boot' + {'aarch64': 'aa64', 'x86_64': 'x64'}[architecture] + '.efi'
            relative = Path('usr/lib/systemd/boot/efi') / name
            original_mount = Path(podman('image', 'mount', raw).strip())
            try:
                original_bytes = (original_mount / relative).read_bytes()
                assert not (original_mount / Path(str(relative) + '.signed')).exists()
            finally:
                podman('image', 'unmount', raw)
            signed_mount = Path(podman('image', 'mount', signed).strip())
            try:
                signed_bytes = (signed_mount / Path(str(relative) + '.signed')).read_bytes()
                assert (signed_mount / Path(str(relative) + '.signed')).stat().st_mtime == EPOCH
                assert (signed_mount / relative).read_bytes() == original_bytes
                assert signed_bytes != original_bytes
            finally:
                podman('image', 'unmount', signed)
            # Hold the already authenticated signing output constant to test
            # layer reproducibility independently of a provider's signature
            # timestamps. This fixture signer and payload stay outside contexts.
            fixed_payload = root / 'fixed-signing-output.efi'
            fixed_payload.write_bytes(signed_bytes)
            fixed_signer = root / 'fixed-output-signer'
            fixed_signer.write_text(
                '#!/usr/bin/python3\nimport shutil,sys\n'
                'assert len(sys.argv) == 4 and sys.argv[1] == "sign-efi"\n'
                f'shutil.copyfile({str(fixed_payload)!r}, sys.argv[3])\n')
            fixed_signer.chmod(0o700)
            repeated = produce(['--signer', str(fixed_signer)])
            if repeated['signed_rootfs_id'] != signed:
                images.append(repeated['signed_rootfs_id'])
            assert repeated['signed_rootfs_id'] == signed, 'identical signed bytes produced a different layer'
            produce(['--rootfs-id', signed], error='already has a signed loader companion')

            consumer = root / 'consumer'
            consumer.mkdir()
            (consumer / 'Containerfile').write_text(
                'FROM scratch AS rootfs\nRUN ["/must-not-rebuild-rootfs"]\n'
                'FROM rootfs AS final\n'
                f'COPY --from=rootfs /{relative}.signed /copied-signed-loader.efi\n')
            reused_iid = root / 'reused.iid'
            podman('build', '--pull=never', '--network=none', '--skip-unused-stages=true', '--target', 'final',
                   '--build-context', f'rootfs=container-image://{signed}',
                   '--iidfile', str(reused_iid), str(consumer))
            reused = reused_iid.read_text().strip()
            images.append(reused)
            mount = Path(podman('image', 'mount', reused).strip())
            try:
                assert (mount / 'copied-signed-loader.efi').read_bytes() == signed_bytes
                assert (mount / relative).read_bytes() == original_bytes
            finally:
                podman('image', 'unmount', reused)

            # The producer must refuse /usr mutation when UKIs already exist.
            (consumer / 'uki').write_bytes(b'sealed-image guard fixture')
            (consumer / 'Containerfile').write_text('FROM rootfs\nCOPY uki /boot/EFI/Linux/fixture.efi\n')
            sealed_iid = root / 'sealed.iid'
            podman('build', '--pull=never', '--network=none', '--build-context',
                   f'rootfs=container-image://{raw}', '--iidfile', str(sealed_iid), str(consumer))
            sealed = sealed_iid.read_text().strip()
            images.append(sealed)
            produce(['--rootfs-id', sealed], error='unsealed rootfs stage')
            versions = podman('run', '--rm', '--pull=never', '--network=none', '--entrypoint', '/usr/bin/rpm',
                             signed, '-q', 'python3', 'systemd-boot-unsigned')
            print(json.dumps({'result': 'pass', 'proof': 'minimal native pre-seal producer and stage reuse only',
                              'storage': 'explicit private VFS store shared by every producer stage',
                              'architecture': architecture, 'package_versions': versions.splitlines(),
                              'producer': receipt, 'reused_rootfs_id': reused}))
        finally:
            primary = sys.exc_info()[0] is not None
            errors = []
            for image in reversed(images):
                try:
                    podman('image', 'rm', '--no-prune', image)
                except RuntimeError as error:
                    errors.append(str(error))
            if errors:
                if not primary:
                    raise RuntimeError('fixture cleanup failed: ' + '\n'.join(errors))
                print('Additional fixture cleanup failures: ' + '\n'.join(errors), file=sys.stderr)


if __name__ == '__main__':
    main()
