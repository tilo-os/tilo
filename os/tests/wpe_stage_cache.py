#!/usr/bin/env python3
"""Disposable native proof of Buildah remote WPE-layer cache and stage reuse.

Uses tiny fixture scripts, two private VFS stores and a local registry. No WPE
compilation, external cache publication, image installation or devices involved.
"""
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import tempfile
import time
import urllib.request
import uuid

BASE = 'registry.fedoraproject.org/fedora@sha256:61beafd34111e1cb85fb49377ceadeee0a53622dbc20670ed8ca303f0e17ed9c'
REGISTRY = 'docker.io/library/registry@sha256:a3d8aaa63ed8681a604f1dea0aa03f100d5895b6a58ace528858a7b332415373'


def podman(*arguments, env=None):
    result = subprocess.run(['podman', '--remote=false', *arguments], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode:
        raise RuntimeError(f'fixture podman {arguments[0]} failed: {result.stderr[-4096:]}')
    return result.stdout, result.stderr


def private_store(root, name):
    configuration = root / (name + '.conf')
    configuration.write_text('[storage]\ndriver="vfs"\n'
        f'graphroot={json.dumps(str(root / (name + "-graph")))}\n'
        f'runroot={json.dumps(str(root / (name + "-run")))}\n')
    return os.environ | {'CONTAINERS_STORAGE_CONF': str(configuration), 'SOURCE_DATE_EPOCH': '0'}


def main():
    if os.getuid() != 0 or platform.machine() not in ('x86_64', 'aarch64'):
        raise SystemExit('requires native root Linux in disposable CI')
    with tempfile.TemporaryDirectory(prefix='tilo-wpe-cache-fixture-', dir='/var/lib') as temporary:
        root = Path(temporary)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        registry = 'tilo-wpe-cache-' + uuid.uuid4().hex
        cache = f'127.0.0.1:{port}/tilo-fixture/cache'
        podman('run', '--detach', '--name', registry, '--network=host',
               '--env', f'REGISTRY_HTTP_ADDR=127.0.0.1:{port}', REGISTRY)
        try:
            for _ in range(60):
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/v2/', timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    time.sleep(0.25)
            else:
                raise RuntimeError('fixture registry did not become ready')
            context = root / 'context'
            assets = root / 'assets'
            context.mkdir()
            assets.mkdir()
            (context / 'Containerfile').write_text(
                'ARG BASE_REF\nFROM scratch AS external-assets\n'
                'COPY --from=github-assets /input /input\n'
                'FROM ${BASE_REF} AS wpe-builder-1\n'
                'ARG TILO_WPE_SOURCE_DATE_EPOCH=0\nENV SOURCE_DATE_EPOCH=${TILO_WPE_SOURCE_DATE_EPOCH}\n'
                'COPY recipe /recipe\n'
                'RUN --mount=type=cache,target=/wpe-cache '
                '--mount=type=bind,from=external-assets,source=/,target=/assets,ro sh /recipe\n')
            recipe = 'echo WPE_FIXTURE_EXECUTED\nmkdir -p /wpe\ncat /assets/input > /wpe/marker\n'
            (context / 'recipe').write_text(recipe)
            (assets / 'input').write_bytes(b'original frozen asset\n')
            first_env = private_store(root, 'producer')
            second_env = private_store(root, 'consumer')

            def build(name, environment, epoch='0'):
                iidfile = root / (name + '.iid')
                out, err = podman('build', '--layers', '--skip-unused-stages=true',
                    '--network=none', '--tls-verify=false', '--cache-to', cache, '--cache-from', cache,
                    '--target', 'wpe-builder-1', '--iidfile', str(iidfile),
                    '--source-date-epoch', epoch, '--rewrite-timestamp',
                    '--build-arg', f'TILO_WPE_SOURCE_DATE_EPOCH={epoch}',
                    '--build-arg', f'BASE_REF={BASE}', '--build-context', f'github-assets={assets}',
                    str(context), env=environment | {'SOURCE_DATE_EPOCH': epoch})
                return iidfile.read_text().strip(), out + err

            original, cold_log = build('cold', first_env)
            assert 'WPE_FIXTURE_EXECUTED' in cold_log
            warm, warm_log = build('warm', second_env)
            assert warm == original, 'remote cached stage changed immutable identity'
            assert 'Cache pulled from remote' in warm_log, 'fresh store did not retrieve registry cache'
            assert 'WPE_FIXTURE_EXECUTED' not in warm_log, 'warm cache executed compiler fixture'

            (assets / 'unrelated').write_bytes(b'new OS asset\n')
            unrelated, unrelated_log = build('unrelated-asset', second_env)
            assert unrelated == original, 'unrelated asset changed the WPE stage'
            assert 'WPE_FIXTURE_EXECUTED' not in unrelated_log, 'unrelated asset reran the WPE compile'

            (context / 'recipe').write_text(recipe + 'printf recipe-change >> /wpe/marker\n')
            recipe_id, recipe_log = build('recipe-change', second_env)
            assert recipe_id != original and 'WPE_FIXTURE_EXECUTED' in recipe_log
            (context / 'recipe').write_text(recipe)
            (assets / 'input').write_bytes(b'changed frozen asset\n')
            asset_id, asset_log = build('asset-change', second_env)
            assert asset_id != original and 'WPE_FIXTURE_EXECUTED' in asset_log
            (assets / 'input').write_bytes(b'original frozen asset\n')
            epoch_id, epoch_log = build('epoch-change', second_env, '1')
            assert epoch_id != original and 'WPE_FIXTURE_EXECUTED' in epoch_log

            consumer = root / 'os-consumer'
            consumer.mkdir()
            (consumer / 'public-marker').write_text('new run public identity')
            (consumer / 'Containerfile').write_text('FROM scratch AS wpe-builder\n'
                'RUN ["/must-never-recompile"]\nFROM scratch AS final\n'
                'COPY --from=wpe-builder /wpe/ /usr/\nCOPY public-marker /public-marker\n')
            final_id_file = root / 'final.iid'
            podman('build', '--pull=never', '--network=none', '--target', 'final',
                '--source-date-epoch', '1700000001', '--rewrite-timestamp',
                '--iidfile', str(final_id_file), '--build-context', f'wpe-builder=container-image://{warm}',
                str(consumer), env=second_env)
            final_id = final_id_file.read_text().strip()
            mounted = Path(podman('image', 'mount', final_id, env=second_env)[0].strip())
            try:
                assert (mounted / 'usr/marker').read_bytes() == b'original frozen asset\n'
                assert (mounted / 'public-marker').read_text() == 'new run public identity'
            finally:
                podman('image', 'unmount', final_id, env=second_env)
            print(json.dumps({'result': 'pass', 'architecture': platform.machine(),
                'podman': podman('--version')[0].strip(), 'dependency_epoch': 0,
                'wpe_fixture_id': original, 'consumer_id': final_id,
                'checks': ['fresh-store registry hit without execution', 'recipe invalidation',
                    'unrelated asset does not invalidate', 'frozen asset invalidation', 'epoch invalidation',
                    'immutable WPE reuse across release epoch and public identity changes']}))
        finally:
            podman('rm', '--force', registry)


if __name__ == '__main__':
    main()
