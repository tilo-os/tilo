#!/usr/bin/env python3
"""Build and cache only the pure native WPE stage, returning its local image ID.

No development trust, generation or source-revision labels enter this build.
The epoch belongs to this dependency artifact, not the enclosing OS release.
Registry cache writers are trusted build inputs; never reuse production cache.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile

ARCHITECTURES = {'x86_64': 'amd64', 'aarch64': 'arm64'}


def validate(args):
    jobs = getattr(args, 'jobs', 2)
    if type(jobs) is not int or not 1 <= jobs <= 64:
        raise ValueError('WPE jobs must be an integer from 1 through 64')
    if args.architecture not in ARCHITECTURES:
        raise ValueError('unsupported WPE architecture')
    if (not re.fullmatch(r'0|[1-9][0-9]{0,11}', args.epoch)
            or int(args.epoch) > 253402300799):
        raise ValueError('WPE epoch must be canonical Unix seconds through year 9999')
    cache = getattr(args, 'cache', None)
    if cache is not None and not re.fullmatch(
            r'registry\.tilo\.so/tilo/tilo-development-wpe-cache-(?:[a-z0-9-]+)?'
            + re.escape(args.architecture), cache):
        raise ValueError('WPE cache must be a dedicated development repository for this architecture')
    if not re.fullmatch(r'registry\.tilo\.so/tilo/tilo-wpe-buildroot@sha256:[0-9a-f]{64}', args.wpe):
        raise ValueError('WPE dependency input must be a pinned WPE buildroot')


def build_command(args, iidfile):
    validate(args)
    command = ['podman', '--remote=false', 'build', '--layers', '--skip-unused-stages=true',
               '--network=none', '--target', 'wpe-builder-1', '--iidfile', str(iidfile),
               '--source-date-epoch', args.epoch, '--rewrite-timestamp',
               '--build-context', f'github-assets={args.assets}',
               '--build-arg', f'WPE_BUILDROOT_REF={args.wpe}',
               '--build-arg', f'TILO_WPE_SOURCE_DATE_EPOCH={args.epoch}',
               '--build-arg', f'WPE_BUILD_JOBS={getattr(args, "jobs", 2)}',
               '-f', str(args.repo / 'os/Containerfile.wpe'), str(args.repo)]
    cache = getattr(args, 'cache', None)
    if cache is not None:
        insert_at = command.index('--network=none') + 1
        command[insert_at:insert_at] = ['--cache-from', cache, '--cache-to', cache]
    return command


def image_identity(raw, expected, architecture):
    images = json.loads(raw)
    if not isinstance(images, list) or len(images) != 1:
        raise ValueError('expected one WPE stage image')
    image = images[0]
    if (not re.fullmatch(r'sha256:[0-9a-f]{64}', expected)
            or image.get('Id', '').removeprefix('sha256:') != expected.removeprefix('sha256:')
            or image.get('Architecture') != ARCHITECTURES[architecture] or image.get('Os') != 'linux'):
        raise ValueError('WPE stage immutable identity or native architecture differs')
    return expected


def bounded_lines(stream):
    """Read at most 4097 bytes at once; discard overlong records entirely."""
    while chunk := stream.readline(4097):
        if len(chunk) > 4096:
            while not chunk.endswith(b'\n'):
                chunk = stream.readline(4097)
                if not chunk:
                    break
            continue
        yield chunk.decode(errors='replace').rstrip('\r\n')


def compiler_failure_lines(stream):
    """Allow bounded compiler records; registry/authentication errors stay private."""
    lines = []
    context = 0
    for text in bounded_lines(stream):
        compiler = bool(re.search(r'[^\s]+\.(?:c|cc|cpp|cxx|h|hpp):[0-9]+(?::[0-9]+)?: (?:fatal )?error:', text)
                        or re.search(r'meson\.build:[0-9]+:[0-9]+: ERROR:', text)
                        or re.search(r'(?:^|/)ld(?:\.lld|\.bfd)?:.*(?:undefined reference|cannot find)', text)
                        or re.search(r'\.(?:c|cc|cpp|cxx):.*undefined reference', text)
                        or re.search(r'^(?:/[^\s]+/)?(?:gcc|g\+\+|cc|c\+\+): fatal error:', text)
                        or text.startswith('collect2: error: ld returned'))
        if text.startswith(('CMake Error at ', 'CMake Error:', 'CMake Error in ')):
            context = 8
        if compiler or context or text.startswith(('ninja: error:', 'ninja: build stopped:')):
            if not re.search(r'(?i)authorization|credential|password|bearer|token|registry|https?://', text):
                lines.append(text)
                lines = lines[-80:]
        context = max(0, context - 1)
    return lines


def prepare(args):
    validate(args)
    if platform.system() != 'Linux' or platform.machine() != args.architecture or os.getuid() != 0:
        raise ValueError('WPE stage preparation requires already-root native Linux')
    if any(os.environ.get(name) for name in ('CONTAINER_HOST', 'CONTAINER_CONNECTION', 'CONTAINER_SSHKEY', 'DOCKER_HOST')):
        raise ValueError('WPE stage cache requires local Podman storage')
    # Preserve explicit local Podman storage/auth selection. No inherited build
    # arguments are forwarded. Recipe and mounts determine the cache key.
    environment = os.environ | {'SOURCE_DATE_EPOCH': args.epoch}
    with tempfile.TemporaryDirectory(prefix='tilo-wpe-stage-') as temporary:
        iidfile = Path(temporary) / 'image.id'
        # Keep potentially large compiler stderr on the caller's measured
        # TMPDIR filesystem, not in a PIPE and duplicate decoded string.
        with (Path(temporary) / 'private-build.stderr').open('w+b') as build_errors:
            try:
                subprocess.run(build_command(args, iidfile), env=environment, check=True,
                               stdout=sys.stderr, stderr=build_errors)
            except subprocess.CalledProcessError:
                build_errors.flush()
                build_errors.seek(0)
                for line in compiler_failure_lines(build_errors):
                    print('TILO_WPE_COMPILER_DIAGNOSTIC ' + line, file=sys.stderr)
                raise
        identity = iidfile.read_text().strip()
        raw = subprocess.check_output(['podman', '--remote=false', 'image', 'inspect', identity],
                                      env=environment, stderr=subprocess.PIPE)
        return image_identity(raw, identity, args.architecture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--jobs', type=int, default=2)
    for name in ('architecture', 'epoch', 'wpe'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--cache')
    for name in ('repo', 'assets'):
        parser.add_argument('--' + name, required=True, type=Path)
    try:
        print(prepare(parser.parse_args()))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'WPE stage preparation failed: {error}\n')


if __name__ == '__main__':
    main()
