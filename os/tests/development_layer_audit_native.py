#!/usr/bin/env python3
"""Real Podman archive/history proof in disposable native CI, with scratch images.

No image program executes and no base image is pulled. The synthetic private
sentinel and PEM envelope have no signing authority and never enter receipts.
"""
import base64
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile
import uuid

DEV = Path(__file__).resolve().parents[1] / 'dev'
spec = importlib.util.spec_from_file_location('layer_audit', DEV / 'audit-rootfs-layers.py')
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


def run(*arguments):
    result = subprocess.run(['podman', '--remote=false', *arguments],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    if result.returncode:
        raise RuntimeError('native layer fixture Podman operation failed')
    return result.stdout.decode().strip()


def main():
    if (os.environ.get('TILO_DISPOSABLE_NATIVE_FIXTURE') != '1' or os.getuid() != 0
            or platform.machine() not in ('x86_64', 'aarch64')):
        raise RuntimeError('requires explicit disposable native root CI fixture')
    if any(os.environ.get(name) for name in ('CONTAINER_HOST', 'CONTAINER_CONNECTION',
                                           'CONTAINER_SSHKEY', 'DOCKER_HOST')):
        raise RuntimeError('remote engines are unsupported')
    nonce = uuid.uuid4().hex
    containers, images = [], []
    with tempfile.TemporaryDirectory(prefix='tilo-layer-native-', dir='/var/lib') as temporary:
        directory = Path(temporary)
        try:
            context = directory / 'context'
            context.mkdir()
            spki = bytes.fromhex('302a300506032b6570032100') + bytes(range(32))
            (context / 'public.pem').write_text('-----BEGIN PUBLIC KEY-----\n' +
                base64.b64encode(spki).decode() + '\n-----END PUBLIC KEY-----\n')
            (context / 'fixture-id').write_text(nonce)
            (context / 'Containerfile').write_text('FROM scratch\nCOPY public.pem fixture-id /\n')
            iidfile = directory / 'public.iid'
            run('build', '--pull=never', '--network=none', '--no-cache',
                '--iidfile', str(iidfile), '--file', str(context / 'Containerfile'), str(context))
            public_id = iidfile.read_text().strip()
            if not re.fullmatch(r'sha256:[0-9a-f]{64}', public_id):
                raise RuntimeError('invalid fixture image ID')
            images.append(public_id)
            public = audit.audit_image(public_id, timeout=120)
            if public['status'] != 'passed-scoped-scan':
                raise RuntimeError('public-only native image rejected')

            def container_from(image, suffix):
                name = 'tilo-layer-' + nonce + '-' + suffix
                run('create', '--name', name, '--network=none', image, '/never-executed')
                containers.append(name)
                return name

            def mount(name):
                path = Path(run('mount', name))
                if not path.is_absolute() or path.resolve() != path or not path.is_dir():
                    raise RuntimeError('unexpected fixture mount')
                return path

            def commit(name):
                image = run('commit', '--format', 'oci', name)
                if not re.fullmatch(r'(?:sha256:)?[0-9a-f]{64}', image):
                    raise RuntimeError('invalid committed fixture ID')
                image = 'sha256:' + image.removeprefix('sha256:')
                images.append(image)
                return image

            sentinel = b'layer-audit-private-' + os.urandom(24).hex().encode()
            first = container_from(public_id, 'create')
            root = mount(first)
            (root / 'private-sentinel').write_bytes(sentinel)
            (root / 'private-envelope.pem').write_bytes(
                b'-----BEGIN PRIVATE KEY-----\nc3ludGhldGljLW5vdC1hLWtleQ==\n-----END PRIVATE KEY-----\n')
            run('unmount', first)
            with_secret = commit(first)
            # A NEW container is essential: two commits of the same container
            # need not preserve its first writable-layer snapshot as ancestry.
            second = container_from(with_secret, 'delete')
            root = mount(second)
            if (root / 'private-sentinel').read_bytes() != sentinel:
                raise RuntimeError('created layer was not inherited')
            (root / 'private-sentinel').unlink()
            (root / 'private-envelope.pem').unlink()
            run('unmount', second)
            deleted_id = commit(second)
            probe = container_from(deleted_id, 'check')
            root = mount(probe)
            if (root / 'private-sentinel').exists() or (root / 'private-envelope.pem').exists():
                raise RuntimeError('deleted sentinel remains in merged image')
            run('unmount', probe)
            historical = audit.audit_image(deleted_id, [sentinel + b'\n'], timeout=120)
            findings = {finding for layer in historical['layers'] for finding in layer['findings']}
            if (historical['status'] != 'rejected-private-material'
                    or findings != {'provided-private-material', 'private-pem-envelope'}
                    or len(historical['layers']) < len(public['layers']) + 2
                    or historical['layers'][-1]['findings']):
                raise RuntimeError('historical private material was not detected correctly')
            print(json.dumps({'schema': 1, 'native_architecture': platform.machine(),
                'public_image': public, 'deleted_material_image': historical,
                'image_programs_executed': False, 'merged_private_files_absent': True,
                'scope': 'real scratch-image Docker archive and historical-layer detection only'}, sort_keys=True))
        finally:
            for name in reversed(containers):
                for command in (['unmount', name], ['rm', '--force', name]):
                    try:
                        subprocess.run(['podman', '--remote=false', *command],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
                    except subprocess.TimeoutExpired:
                        pass
            for image in reversed(images):
                try:
                    subprocess.run(['podman', '--remote=false', 'rmi', image],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
                except subprocess.TimeoutExpired:
                    pass


if __name__ == '__main__':
    try:
        main()
    except (audit.AuditError, RuntimeError) as error:
        # These errors contain only our fixed categories, never tool stderr,
        # paths supplied by archive members, or matched private bytes.
        raise SystemExit(f'native layer fixture rejected: {error}') from None
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        raise SystemExit('native layer audit fixture failed; private diagnostics suppressed') from None
