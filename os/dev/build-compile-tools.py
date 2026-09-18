#!/usr/bin/env python3
"""Build the authoritative tools recipe, retaining optional inline registry cache."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

REPOSITORY = 'code.sandbox.example/qos/tilo-compile-tools-cache'


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
    if optional(['docker', 'pull', cache]) is not None:
        inspected = optional(['docker', 'image', 'inspect', '--format', '{{json .RepoDigests}}', cache], capture=True)
        if inspected is not None:
            try:
                digests = json.loads(inspected.stdout)
                matches = [ref for ref in digests if isinstance(ref, str) and
                           re.fullmatch(re.escape(REPOSITORY) + r'@sha256:[0-9a-f]{64}', ref)]
                if len(matches) == 1:
                    arguments = ['--cache-from', matches[0]]
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
