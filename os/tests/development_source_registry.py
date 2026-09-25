#!/usr/bin/env python3
"""Real disposable Cosign/registry/Podman acceptance fixture; run in podman unshare.

Requires cosign, podman and tilo-crypto. Set TMPDIR and CONTAINERS_STORAGE_CONF to
private temporary storage if the host must not retain fixture image layers.
No host disk, firmware, production repository or production key is used.
"""
import importlib.util
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid
from types import SimpleNamespace
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
REGISTRY = 'docker.io/library/registry@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373'


def run(*args, env=None, success=True):
    result = subprocess.run(args, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if (result.returncode == 0) != success:
        # Fixture generates only disposable keys, but still never print tool logs.
        raise RuntimeError(f'{args[0]} {args[1]} unexpected exit {result.returncode}')
    return result.stdout.decode()


def main():
    if os.getuid() != 0:
        raise SystemExit('run fixture inside podman unshare (or as root)')
    native = platform.machine()
    if native not in ('aarch64', 'x86_64'):
        raise SystemExit('fixture requires a native supported architecture')
    with tempfile.TemporaryDirectory(prefix='tilo-development-registry-') as directory:
        root = Path(directory)
        env = os.environ.copy()
        env['COSIGN_PASSWORD'] = ''
        env.pop('COSIGN_REPOSITORY', None)
        env.pop('REGISTRY_AUTH_FILE', None)
        auth = root / 'auth'
        auth.mkdir()
        (auth / 'config.json').write_text('{"auths":{}}')
        env['DOCKER_CONFIG'] = str(auth)
        env['REGISTRY_AUTH_FILE'] = str(auth / 'config.json')
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        target = f'127.0.0.1:{port}/tilo-development/os:fixture'
        registry_name = 'tilo-app-fixture-' + uuid.uuid4().hex
        images = []
        try:
            run('podman', 'run', '--detach', '--name', registry_name, '--network=host',
                '--env', f'REGISTRY_HTTP_ADDR=127.0.0.1:{port}', REGISTRY, env=env)
            for _ in range(60):
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/v2/', timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    time.sleep(0.25)
            else:
                raise RuntimeError('disposable registry did not become ready')
            key = root / 'cosign'
            run('cosign', 'generate-key-pair', '--output-key-prefix', str(key), env=env)
            run('/usr/local/bin/tilo-crypto', 'generate-tpm-authority-key',
                str(root / 'pcr.key'), str(root / 'pcr.pub'))
            run('/usr/local/bin/tilo-crypto', 'generate-secure-boot-cert',
                'Tilo Disposable Fixture', str(root / 'efi.key'), str(root / 'efi.crt'))
            (root / 'owner.pub').write_text('ab' * 32 + '\n')
            bundle = root / 'public'
            result = json.loads(run('python3', str(DEV / 'development-trust.py'), 'create',
                                    '--cosign-public', str(key) + '.pub', '--pcr-public', str(root / 'pcr.pub'),
                                    '--efi-certificate', str(root / 'efi.crt'), '--registry-owner', str(root / 'owner.pub'),
                                    '--output', str(bundle)))
            digest = result['manifest_sha256']
            context = root / 'context'
            image_root = context / 'rootfs'
            run('python3', str(DEV / 'install-public-trust.py'), '--source', str(bundle), '--official', str(bundle),
                '--destination', str(image_root / 'usr/share/tilo'), '--mode', 'development',
                '--manifest-sha256', digest, '--registry-owner', 'ab' * 32,
                '--target-ref', target, '--image-root', str(image_root))
            revision = 'b' * 40
            (context / 'Containerfile').write_text('FROM scratch\nCOPY --chown=0:0 rootfs/ /\n'
                                                 f'LABEL org.opencontainers.image.revision={revision}\n'
                                                 'ENTRYPOINT ["/image-code-must-never-execute"]\n')
            run('podman', 'build', '--network=none', '--pull=never', '--iidfile', str(root / 'iid'), '-t', target, str(context), env=env)
            images.append((root / 'iid').read_text().strip())
            run('podman', 'push', '--tls-verify=false', '--digestfile', str(root / 'digest'), target, env=env)
            source = target.rsplit(':', 1)[0] + '@' + (root / 'digest').read_text().strip()
            command = ['python3', str(DEV / 'verify-development-source.py'), '--source', source,
                       '--target', target, '--architecture', native, '--revision', revision,
                       '--public-bundle', str(bundle), '--manifest-sha256', digest, '--offline-key-signature']
            run(*command, env=env, success=False)
            run('cosign', 'sign', '--yes', '--tlog-upload=false', '--allow-http-registry',
                '--key', str(key) + '.key', source, env=env)
            receipt = json.loads(run(*command, env=env))
            assert receipt['source'] == source and receipt['manifest_sha256'] == digest
            assert receipt['transparency_log_required'] is False
            expected_id = 'sha256:' + images[0].removeprefix('sha256:')
            assert receipt['image_id'] == expected_id
            sys.path.insert(0, str(DEV))
            spec = importlib.util.spec_from_file_location('native_source', DEV / 'verify-development-source.py')
            verifier = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(verifier)
            args = SimpleNamespace(source=source, target=target, architecture=native,
                revision=revision, public_bundle=bundle, manifest_sha256=digest,
                offline_key_signature=True)
            with patch.dict(os.environ, env, clear=True):
                with verifier.verified_source(args) as verified:
                    assert verified.image_id == expected_id
                    snapshot = verified.public_bundle
                    assert snapshot != bundle and snapshot.is_dir()
                    assert (verified.root / 'usr/share/tilo/development-trust/manifest.json').is_file()
                    assert (snapshot / 'manifest.json').read_bytes() == (bundle / 'manifest.json').read_bytes()
                assert not snapshot.exists()
            for option, value in (('--revision', 'c' * 40),
                                  ('--architecture', 'x86_64' if native == 'aarch64' else 'aarch64'),
                                  ('--manifest-sha256', 'd' * 64)):
                changed = command.copy()
                changed[changed.index(option) + 1] = value
                run(*changed, env=env, success=False)
            wrong_key = root / 'wrong-cosign'
            run('cosign', 'generate-key-pair', '--output-key-prefix', str(wrong_key), env=env)
            wrong_bundle = root / 'wrong-public'
            wrong = json.loads(run('python3', str(DEV / 'development-trust.py'), 'create',
                                   '--cosign-public', str(wrong_key) + '.pub', '--pcr-public', str(root / 'pcr.pub'),
                                   '--efi-certificate', str(root / 'efi.crt'), '--registry-owner', str(root / 'owner.pub'),
                                   '--output', str(wrong_bundle)))
            changed = command.copy()
            changed[changed.index('--public-bundle') + 1] = str(wrong_bundle)
            changed[changed.index('--manifest-sha256') + 1] = wrong['manifest_sha256']
            run(*changed, env=env, success=False)
            # The Cosign key remains valid, but independent public trust differs
            # from the signed image. This must reach and fail mounted inspection.
            (root / 'owner.pub').write_text('cd' * 32 + '\n')
            changed_bundle = root / 'changed-owner-public'
            changed_trust = json.loads(run('python3', str(DEV / 'development-trust.py'), 'create',
                                           '--cosign-public', str(key) + '.pub', '--pcr-public', str(root / 'pcr.pub'),
                                           '--efi-certificate', str(root / 'efi.crt'), '--registry-owner', str(root / 'owner.pub'),
                                           '--output', str(changed_bundle)))
            changed = command.copy()
            changed[changed.index('--public-bundle') + 1] = str(changed_bundle)
            changed[changed.index('--manifest-sha256') + 1] = changed_trust['manifest_sha256']
            run(*changed, env=env, success=False)
            print(json.dumps({'result': 'pass', 'architecture': native,
                              'checks': ['unsigned rejected', 'signed source accepted',
                                         'wrong revision rejected', 'wrong architecture rejected',
                                         'wrong manifest rejected', 'wrong signing key rejected',
                                         'signed embedded trust mismatch rejected',
                                         'immutable ID and held mount handoff', 'no image execution']}))
        finally:
            subprocess.run(['podman', 'rm', '--force', registry_name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for image in images:
                subprocess.run(['podman', 'rmi', '--force', image], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
