#!/usr/bin/env python3
"""Real Podman/dracut trust-rebind lifecycle with packaged kernel modules.

Uses the existing TLS/Cosign retention path and explicitly synthetic application
payloads. No kernel or application compilation. Observes the production
`rebind-development-rootfs.transform` path: container capabilities, source
identity, dracut exit, regenerated PCR public key, pending-marker removal,
timestamp normalization, immutable COPY projection and preserved runtime config.

Packaged modules are installed into an installroot with the disposable fixture's
outer network, then projected with `podman build --network=none`. Nested image
code never runs. Requires the disposable native CI container and fails closed
when the production path cannot be observed.
"""
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

from tls_registry_fixture import tls_registry, run

HERE = Path(__file__).resolve().parent
DEV = HERE.parents[0] / 'dev'
ROLLBACK_MODULE = HERE.parents[0] / 'usr/lib/dracut/modules.d/91tilo-rollback'
sys.path.insert(0, str(DEV))

spec = importlib.util.spec_from_file_location('retention', DEV / 'retain-development-rootfs.py')
retention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retention)
spec = importlib.util.spec_from_file_location('transform', DEV / 'rebind-development-rootfs.py')
transform = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transform)


def write_stub(path, name):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        f'#!/bin/bash\n# synthetic {name} fixture payload — never a real Tilo binary\nexit 0\n')
    path.chmod(0o755)


def install_synthetic_rollback(image_root):
    if not ROLLBACK_MODULE.is_dir():
        raise ValueError('packaged tilo-rollback dracut module missing from fixture tree')
    destination = image_root / 'usr/lib/dracut/modules.d/91tilo-rollback'
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(ROLLBACK_MODULE, destination)
    for relative in (
            'usr/bin/tilo-rollback',
            'usr/libexec/tilo-rollback-initrd-unlock',
            'usr/libexec/tilo-root-discover',
            'usr/lib/systemd/system-generators/tilo-root-generator'):
        write_stub(image_root / relative, relative)
    app = image_root / 'usr/share/tilo-fixture-apps/synthetic-payload.txt'
    app.parent.mkdir(parents=True, exist_ok=True)
    app.write_text('synthetic application payload for trust-rebind lifecycle\n')


def packaged_kernel_packages():
    return ('kernel-core', 'dracut', 'cryptsetup', 'tpm2-tools', 'tpm2-tss',
            'plymouth', 'systemd', 'kmod', 'binutils', 'coreutils', 'bash',
            'util-linux', 'grep', 'gawk', 'findutils', 'sed', 'xz', 'zstd',
            'gzip', 'cpio', 'squashfs-tools')


def install_packaged_rootfs(image_root):
    """Install packaged kernel modules with the outer fixture network only."""
    image_root.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        ['dnf', '-y', '--use-host-config', f'--installroot={image_root}',
         '--setopt=install_weak_deps=False', '--setopt=tsflags=nodocs',
         'install', *packaged_kernel_packages()],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=900)
    if result.returncode:
        detail = result.stdout.decode(errors='replace')[-4096:]
        raise RuntimeError('packaged-kernel installroot failed: ' + detail)
    # Match the production image: dracut's ldconfig -r rejects Fedora's
    # relative include pattern even though it names the same directory.
    config = image_root / 'etc/ld.so.conf'
    original = config.read_text()
    normalized = original.replace('include ld.so.conf.d/*.conf\n',
                                  'include /etc/ld.so.conf.d/*.conf\n')
    if normalized != original:
        config.write_text(normalized)
    logging_config = image_root / 'etc/dracut.conf.d/99-tilo-build-logging.conf'
    logging_config.parent.mkdir(parents=True, exist_ok=True)
    logging_config.write_text('sysloglvl=0\n')
    modules = sorted(path.name for path in (image_root / 'usr/lib/modules').iterdir()
                     if path.is_dir())
    if len(modules) != 1:
        raise ValueError('packaged-kernel installroot must expose exactly one modules tree')
    kver = modules[0]
    vmlinuz = image_root / 'usr/lib/modules' / kver / 'vmlinuz'
    if not vmlinuz.is_file():
        raise ValueError('packaged kernel vmlinuz missing from installroot')
    initramfs = image_root / 'usr/lib/modules' / kver / 'initramfs.img'
    initramfs.write_bytes(b'synthetic placeholder initramfs\n')
    if not (image_root / 'usr/bin/dracut').is_file():
        raise ValueError('packaged dracut missing from installroot')
    return kver


def inspect_create_capabilities(create_args):
    required = ('--cap-drop=all', '--security-opt=no-new-privileges', '--network=none',
                '--pull=never', '--user=0', '--entrypoint=/usr/bin/dracut')
    missing = [flag for flag in required if flag not in create_args]
    if missing:
        raise ValueError('production create arguments lost required capability bounds: '
                         + ', '.join(missing))
    if '--privileged' in create_args or '--volume' in create_args:
        raise ValueError('production create arguments gained privileged escape')


def podman(*arguments):
    result = subprocess.run(['podman', '--remote=false', *arguments],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=900)
    if result.returncode:
        detail = (result.stderr or result.stdout).decode(errors='replace')[-4096:]
        raise RuntimeError(f'podman {" ".join(arguments[:3])} failed: {detail}')
    return result.stdout.decode()


def main():
    native = platform.machine()
    if (os.environ.get('TILO_DISPOSABLE_NATIVE_FIXTURE') != '1' or os.getuid() != 0
            or native not in ('x86_64', 'aarch64')):
        raise SystemExit('requires disposable native root CI container')
    if not any(Path(path).exists() for path in ('/.dockerenv', '/run/.containerenv')):
        raise ValueError('fixture must observe production path inside disposable CI container')
    if shutil.which('cosign', path=transform.importer.TOOL_PATH) != '/usr/local/bin/cosign':
        raise ValueError('importer cannot discover the configured pinned Cosign executable')
    for tool in ('podman', 'skopeo', 'dnf'):
        if not shutil.which(tool):
            raise ValueError(f'required fixture tool missing: {tool}')

    revision = 'b' * 40
    generation, epoch = 7, 1700000000
    images = []
    with tls_registry() as authority, tempfile.TemporaryDirectory(
            prefix='tilo-rebind-native-', dir='/var/lib') as temporary:
        root = Path(temporary)
        auth = root / 'auth.json'
        auth.write_text('{"auths":{}}')
        auth.chmod(0o600)
        environment = {name: value for name, value in os.environ.items()
                       if not name.startswith(('COSIGN_', 'CONTAINER_', 'DOCKER_', 'REGISTRY_'))}
        environment.pop('SSL_CERT_FILE', None)
        environment.pop('SSL_CERT_DIR', None)
        environment.update(COSIGN_PASSWORD='', REGISTRY_AUTH_FILE=str(auth))
        with patch.dict(os.environ, environment, clear=True):
            try:
                old_key = root / 'cosign-old'
                new_key = root / 'cosign-new'
                run('cosign', 'generate-key-pair', '--output-key-prefix', str(old_key))
                run('cosign', 'generate-key-pair', '--output-key-prefix', str(new_key))
                Path(str(old_key) + '.key').chmod(0o600)
                Path(str(new_key) + '.key').chmod(0o600)
                old_signer = json.loads(run('python3', str(DEV / 'setup-development-signer.py'),
                                            str(root / 'signer-old')))
                new_signer = json.loads(run('python3', str(DEV / 'setup-development-signer.py'),
                                            str(root / 'signer-new')))
                (root / 'owner-old.pub').write_text('ab' * 32 + '\n')
                (root / 'owner-new.pub').write_text('cd' * 32 + '\n')
                old_public = root / 'public-old'
                new_public = root / 'public-new'
                old_manifest = json.loads(run(
                    'python3', str(DEV / 'development-trust.py'), 'create',
                    '--cosign-public', str(old_key) + '.pub',
                    '--pcr-public', old_signer['pcr_public'],
                    '--efi-certificate', old_signer['efi_certificate'],
                    '--registry-owner', str(root / 'owner-old.pub'),
                    '--output', str(old_public)))['manifest_sha256']
                new_manifest = json.loads(run(
                    'python3', str(DEV / 'development-trust.py'), 'create',
                    '--cosign-public', str(new_key) + '.pub',
                    '--pcr-public', new_signer['pcr_public'],
                    '--efi-certificate', new_signer['efi_certificate'],
                    '--registry-owner', str(root / 'owner-new.pub'),
                    '--output', str(new_public)))['manifest_sha256']
                if old_manifest == new_manifest:
                    raise ValueError('fixture failed to produce distinct public trust identities')
                old_pcr = (old_public / 'tpm2-pcr-signing.public.pem').read_bytes()
                new_pcr = (new_public / 'tpm2-pcr-signing.public.pem').read_bytes()
                if old_pcr == new_pcr:
                    raise ValueError('fixture failed to regenerate a distinct PCR public key')

                target = f'registry.tilo.so/tilo/tilo-development:rebind-fixture-{native}'
                new_target = f'registry.tilo.so/tilo/tilo-development:rebind-fixture-new-{native}'
                context = root / 'context'
                image_root = context / 'rootfs'
                # Outer-network package fetch only; image projection stays offline.
                kver = install_packaged_rootfs(image_root)
                run('python3', str(DEV / 'install-public-trust.py'),
                    '--source', str(old_public), '--official', str(old_public),
                    '--destination', str(image_root / 'usr/share/tilo'),
                    '--mode', 'development', '--manifest-sha256', old_manifest,
                    '--registry-owner', 'ab' * 32, '--target-ref', target,
                    '--image-root', str(image_root))
                generation_path = image_root / 'usr/lib/tilo/generation'
                generation_path.parent.mkdir(parents=True, exist_ok=True)
                generation_path.write_text(f'{generation}\n')
                # Production transform replaces these records in place; they must
                # already exist as regular files on the retained raw rootfs.
                (image_root / 'usr/lib/tilo/initramfs-modules.txt').write_text('placeholder\n')
                (image_root / 'usr/lib/tilo/initramfs-omitted-drivers.txt').write_text('placeholder\n')
                (image_root / 'usr/lib/systemd/boot/efi').mkdir(parents=True, exist_ok=True)
                install_synthetic_rollback(image_root)
                if 'synthetic application payload' not in (
                        image_root / 'usr/share/tilo-fixture-apps/synthetic-payload.txt').read_text():
                    raise ValueError('synthetic application payload missing from packaged rootfs')
                (context / 'Containerfile').write_text(
                    'FROM scratch\n'
                    'COPY rootfs/ /\n'
                    f'LABEL org.opencontainers.image.revision={revision}\n'
                    'ENTRYPOINT ["/image-code-must-never-execute"]\n'
                    'CMD ["/image-code-must-never-execute"]\n')
                podman('build', '--network=none', '--pull=never', '--iidfile',
                       str(root / 'iid'), str(context))
                image_id = 'sha256:' + (root / 'iid').read_text().strip().removeprefix('sha256:')
                images.append(image_id)

                handoff = {
                    'schema': 1, 'purpose': 'tilo-development-raw-rootfs-handoff',
                    'revision': revision, 'architecture': native,
                    'public_manifest_sha256': old_manifest, 'target_update_ref': target,
                    'generation': generation, 'source_date_epoch': epoch,
                    'raw_rootfs_id': image_id, 'installable': False, 'sealed': False,
                    'published': False,
                    'validation': 'captured build inputs only; independently revalidate before reuse',
                }
                handoff_path = root / 'handoff.json'
                handoff_path.write_text(json.dumps(handoff))
                retain_args = SimpleNamespace(
                    receipt=handoff_path, private_key=Path(str(old_key) + '.key'),
                    public_bundle=old_public, revision=revision, architecture=native,
                    manifest_sha256=old_manifest, target=target, generation=generation,
                    source_date_epoch=epoch)
                repository = authority + '/tilo-development-rootfs'
                with patch.object(retention, 'REPOSITORY', repository):
                    receipt = retention.retain(retain_args)

                retained_path = root / 'retained.json'
                retained_path.write_text(json.dumps(receipt))
                create_args = transform.create_arguments(image_id, kver, epoch, root / 'hooks')
                inspect_create_capabilities(create_args)

                transform_args = SimpleNamespace(
                    receipt=retained_path, public_bundle=old_public,
                    new_public_bundle=new_public, revision=revision, architecture=native,
                    manifest_sha256=old_manifest, target=target,
                    new_manifest_sha256=new_manifest, new_target=new_target,
                    new_owner='cd' * 32, generation=generation, source_date_epoch=epoch)
                with patch.object(transform.importer.retention, 'REPOSITORY', repository):
                    result = transform.transform(transform_args)
                images.append(result['rootfs_id'])

                if result['source_id'] != image_id:
                    raise ValueError('exact source identity was not preserved')
                if result['manifest_sha256'] != new_manifest:
                    raise ValueError('rebound manifest identity differs')
                if result['target_update_ref'] != new_target:
                    raise ValueError('rebound target differs')
                if result['source_date_epoch'] != epoch:
                    raise ValueError('timestamp normalization input was not preserved')
                if result['installable'] or result['boot_proven'] or result['sealed']:
                    raise ValueError('lifecycle receipt made a boot or install claim')

                proof = result['initramfs']['artifact']
                if proof.get('pcr_public_sha256') != hashlib.sha256(new_pcr).hexdigest():
                    raise ValueError('regenerated PCR public key was not observed in initramfs')
                if proof.get('pcr_public_sha256') == hashlib.sha256(old_pcr).hexdigest():
                    raise ValueError('initramfs still carried the pre-rebind PCR public key')

                final = json.loads(podman('image', 'inspect', result['rootfs_id']))[0]
                created = datetime.fromisoformat(final['Created'].replace('Z', '+00:00'))
                if int(created.replace(tzinfo=timezone.utc).timestamp()) != epoch:
                    raise ValueError(f'timestamp normalization failed: {final["Created"]}')
                # Newest layer first. Production projection is exactly one immutable
                # COPY onto the retained raw rootfs; buildah renders it as #(nop) COPY.
                history_lines = [line.strip() for line in podman(
                    'image', 'history', '--no-trunc', '--format', '{{.CreatedBy}}',
                    result['rootfs_id']).splitlines() if line.strip()]
                if not history_lines:
                    raise ValueError('image history was empty')
                newest = history_lines[0].lower()
                if '#(nop)' not in newest or 'copy' not in newest:
                    raise ValueError('immutable COPY projection layer was not observed: '
                                     + history_lines[0][:512])
                if any('#(nop)' not in line.lower() and '/bin/sh -c' in line.lower()
                       for line in history_lines[:1]):
                    raise ValueError('projection newest layer executed a RUN')

                out_root = Path(podman('image', 'mount', result['rootfs_id']).strip())
                try:
                    pending = out_root / transform.completion.PENDING
                    if pending.exists() or pending.is_symlink():
                        raise ValueError('pending rebind marker was not removed')
                    verified = out_root / transform.completion.VERIFIED
                    if not verified.is_file():
                        raise ValueError('verified initramfs receipt missing')
                    installed_pcr = (out_root / 'usr/share/tilo/tpm2-pcr-signing.public.pem').read_bytes()
                    if installed_pcr != new_pcr:
                        raise ValueError('rootfs PCR public key was not rebound')
                    if retention.raw_kernel_pair(out_root) != kver:
                        raise ValueError('output packaged kernel version differs')
                    initramfs = out_root / 'usr/lib/modules' / kver / 'initramfs.img'
                    if not initramfs.is_file() or initramfs.stat().st_size < 1024:
                        raise ValueError('dracut did not produce a real initramfs')
                    modules_txt = (out_root / transform.MODULES).read_text().splitlines()
                    for name in sorted(transform.REQUIRED_MODULES):
                        if name not in modules_txt:
                            raise ValueError(f'dracut module evidence missing {name}')
                    for name in transform.OMITTED_MODULES:
                        if name in modules_txt:
                            raise ValueError(f'dracut included omitted module {name}')
                    omitted = (out_root / 'usr/lib/tilo/initramfs-omitted-drivers.txt').read_text().split()
                    expected = transform.omitted_drivers(receipt.get('hardware_profile'))
                    if omitted != expected:
                        raise ValueError(
                            'omitted driver record differs from production policy: '
                            + repr(omitted) + ' vs ' + repr(expected))
                    payload = (out_root / 'usr/share/tilo-fixture-apps/synthetic-payload.txt').read_text()
                    if 'synthetic application payload' not in payload:
                        raise ValueError('synthetic application payload was not preserved')
                    original = json.loads(podman('image', 'inspect', image_id))[0]
                    for field in transform.RUNTIME_FIELDS:
                        if original.get('Config', {}).get(field) != final.get('Config', {}).get(field):
                            raise ValueError(f'runtime configuration changed: {field}')
                finally:
                    podman('image', 'unmount', result['rootfs_id'])

                print(json.dumps({
                    'result': 'pass',
                    'architecture': native,
                    'kernel_version': kver,
                    'proof': ('packaged-kernel Podman/dracut trust rebind over TLS retention; '
                              'lifecycle only'),
                    'source_id': image_id,
                    'rootfs_id': result['rootfs_id'],
                    'old_manifest_sha256': old_manifest,
                    'new_manifest_sha256': new_manifest,
                    'pcr_public_sha256': proof['pcr_public_sha256'],
                    'source_date_epoch': epoch,
                    'synthetic_application_payload': True,
                    'image_code_executed': False,
                    'boot_proven': False,
                    'installable': False,
                    'checks': [
                        'container capability bounds on production create arguments',
                        'exact retained source identity',
                        'successful dracut exit with required modules',
                        'regenerated PCR public key in initramfs',
                        'pending-marker removal',
                        'timestamp normalization',
                        'immutable COPY projection layer',
                        'preserved runtime configuration',
                        'synthetic application payload retained outside projection surface',
                    ],
                }, sort_keys=True, indent=2))
            finally:
                for image in images:
                    subprocess.run(['podman', '--remote=false', 'rmi', '--force', image],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
