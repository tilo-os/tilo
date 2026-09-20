#!/usr/bin/env python3
"""Build the authoritative tools recipe, retaining optional inline registry cache."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

REPOSITORY = 'code.podesta.ai/qos/tilo-compile-tools-cache'


def fingerprint(recipe, builder, cosign, platform):
    if not re.fullmatch(r'code\.podesta\.ai/qos/[a-z0-9-]+@sha256:[0-9a-f]{64}', builder):
        raise ValueError('immutable builder required')
    if platform not in ('linux/arm64', 'linux/amd64'):
        raise ValueError('unsupported tools platform')
    fields = [hashlib.sha256(recipe).hexdigest(), builder,
              hashlib.sha256(cosign).hexdigest(), platform]
    return hashlib.sha256(json.dumps(fields, separators=(',', ':')).encode()).hexdigest()


def optional(argv, *, capture=False, timeout=180):
    # Cache availability must not prevent a valid authoritative build. Suppress
    # registry diagnostics; no credentials are read or put into command arguments.
    try:
        return subprocess.run(argv, check=True, stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, timeout=timeout)
    except (OSError, subprocess.SubprocessError):
        return None


def build(context, builder, platform, tag):
    context = Path(context)
    key = fingerprint((context / 'Containerfile').read_bytes(), builder,
                      (context / 'cosign').read_bytes(), platform)
    cache = REPOSITORY + ':' + key
    arguments = []
    # Resolve metadata only. Eager `docker pull` extracts every large layer
    # before BuildKit can use the inline cache and may time out needlessly.
    inspected = optional(['docker', 'buildx', 'imagetools', 'inspect', '--format', '{{json .Manifest}}', cache],
                         capture=True, timeout=30)
    if inspected is not None and 0 < len(inspected.stdout) <= 1024 * 1024:
        try:
            manifest = json.loads(inspected.stdout)
            if (isinstance(manifest, dict) and isinstance(manifest.get('digest'), str)
                    and re.fullmatch(r'sha256:[0-9a-f]{64}', manifest['digest'])
                    and type(manifest.get('size')) is int and 0 < manifest['size'] <= 1024 * 1024
                    and manifest.get('mediaType') in (
                        'application/vnd.docker.distribution.manifest.v2+json',
                        'application/vnd.oci.image.manifest.v1+json',
                        'application/vnd.docker.distribution.manifest.list.v2+json',
                        'application/vnd.oci.image.index.v1+json')):
                # The CLI may render manifest bytes; only the registry's
                # descriptor digest is the immutable image identity.
                arguments = ['--cache-from', REPOSITORY + '@' + manifest['digest']]
        except (ValueError, TypeError):
            pass
    # Docker/BuildKit still evaluates the exact recipe and context. The cache
    # key is a lookup hint, never an instruction to bypass the build.
    subprocess.run(['docker', 'build', '--file', str(context / 'Containerfile'),
                    '--tag', tag, '--platform', platform, '--build-arg', 'BUILDER_REF=' + builder,
                    '--build-arg', 'BUILDKIT_INLINE_CACHE=1', *arguments, str(context)], check=True)
    actual = subprocess.check_output(['docker', 'image', 'inspect', '--format', '{{.Os}}/{{.Architecture}}', tag], timeout=30).decode().strip()
    if actual != platform:
        raise ValueError('built tools platform differs')
    published = False
    if optional(['docker', 'tag', tag, cache], timeout=30) is not None:
        published = optional(['docker', 'push', cache], timeout=180) is not None
    print(json.dumps({'cache_key': key, 'cache_imported': bool(arguments), 'cache_published': published}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('context', 'builder', 'platform', 'tag'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    build(args.context, args.builder, args.platform, args.tag)


if __name__ == '__main__':
    main()
