#!/usr/bin/env python3
"""Prove immutable local-image stage overrides in disposable native Podman CI.

All images are scratch images containing fixture text. No image code, registry,
host mount, physical disk or firmware is needed. This is not an OS sealing test.
"""
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import uuid


def podman(*arguments, success=True):
    result = subprocess.run(['podman', '--remote=false', *arguments],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env={'PATH': os.defpath, 'LANG': 'C.UTF-8', 'HOME': '/root'})
    if (result.returncode == 0) != success:
        raise RuntimeError(f'podman {arguments[0]} unexpected exit {result.returncode}: '
                           + result.stderr.decode(errors='replace')[-4096:])
    return result.stdout.decode()


def main():
    if os.getuid() != 0 or platform.machine() not in ('x86_64', 'aarch64'):
        raise SystemExit('run only as root in the disposable native CI container')
    with tempfile.TemporaryDirectory(prefix='tilo-stage-reuse-') as directory:
        root = Path(directory)
        images = []
        tag = 'localhost/tilo-stage-fixture:' + uuid.uuid4().hex

        def build(context, name, *extra, success=True):
            iidfile = root / (name + '.iid')
            podman('build', '--network=none', '--pull=never', '--no-cache',
                   '--skip-unused-stages=true', '--iidfile', str(iidfile),
                   *extra, str(context), success=success)
            if not success:
                assert not iidfile.exists(), 'failed build published an image ID'
                return None
            identity = iidfile.read_text().strip()
            assert re.fullmatch(r'sha256:[0-9a-f]{64}', identity), identity
            images.append(identity)
            return identity

        try:
            source = root / 'source'
            source.mkdir()
            (source / 'Containerfile').write_text(
                'FROM scratch\nCOPY marker /marker\n'
                'LABEL ai.tilo.fixture="immutable-stage-source"\n')
            (source / 'marker').write_bytes(b'original immutable rootfs bytes\n')
            original = build(source, 'original', '--tag', tag)
            (source / 'marker').write_bytes(b'replacement mutable tag bytes\n')
            replacement = build(source, 'replacement', '--tag', tag)
            assert original != replacement
            tagged = json.loads(podman('image', 'inspect', tag))[0]
            assert tagged['Id'].removeprefix('sha256:') == replacement.removeprefix('sha256:')

            consumer = root / 'consumer'
            consumer.mkdir()
            (consumer / 'Containerfile').write_text(
                'FROM scratch AS rootfs\n'
                'RUN ["/this-stage-must-never-execute"]\n'
                'FROM rootfs AS final\n'
                'COPY --from=rootfs /marker /copied-marker\n')
            final = build(consumer, 'final', '--target', 'final',
                          '--build-context', f'rootfs=container-image://{original}')
            info = json.loads(podman('image', 'inspect', final))[0]
            assert info['Architecture'] == {'x86_64': 'amd64', 'aarch64': 'arm64'}[platform.machine()]
            assert info['Labels']['ai.tilo.fixture'] == 'immutable-stage-source'
            mounted = Path(podman('image', 'mount', final).strip())
            try:
                for name in ('marker', 'copied-marker'):
                    path = mounted / name
                    assert path.is_file() and not path.is_symlink()
                    assert path.read_bytes() == b'original immutable rootfs bytes\n'
            finally:
                podman('image', 'unmount', final)

            missing = 'sha256:' + uuid.uuid4().hex + uuid.uuid4().hex
            podman('image', 'exists', missing, success=False)
            build(consumer, 'missing', '--target', 'final', '--build-context',
                  f'rootfs=container-image://{missing}', success=False)
            print(json.dumps({'result': 'pass', 'proof': 'immutable local FROM and COPY stage reuse',
                              'architecture': platform.machine(), 'podman_version': podman('--version').strip(),
                              'original_image_id': original, 'replacement_image_id': replacement,
                              'final_image_id': final,
                              'checks': ['overridden stage never executed', 'moved tag ignored',
                                         'FROM label and file preserved', 'COPY source preserved',
                                         'missing local image rejected without pulling']}))
        finally:
            primary_failure = sys.exc_info()[0] is not None
            cleanup_errors = []
            for identity in reversed(images):
                try:
                    # Do not prune the recorded parent before its own cleanup.
                    podman('image', 'rm', '--no-prune', identity)
                except RuntimeError as error:
                    cleanup_errors.append(str(error))
            if cleanup_errors:
                detail = '\n'.join(cleanup_errors)
                if not primary_failure:
                    raise RuntimeError('fixture image cleanup failed: ' + detail)
                print('Additional fixture cleanup failures: ' + detail, file=sys.stderr)


if __name__ == '__main__':
    main()
