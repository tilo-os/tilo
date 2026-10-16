#!/usr/bin/env python3
"""Bounded, fail-closed helpers for development artifacts in the registry.

Inputs are read with size bounds and without following links, JSON is parsed
strictly, and registry and local images are identified by digest before any
of their content is trusted. Nothing here executes artifact code.
"""
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess

ARCHES = {'x86_64': ('amd64', 62), 'aarch64': ('arm64', 183)}
DIGEST = r'sha256:[0-9a-f]{64}'


def require(condition, message):
    if not condition:
        raise ValueError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read_bounded(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        before = os.fstat(descriptor)
        require(stat.S_ISREG(before.st_mode) and before.st_size <= limit, 'bounded regular input required')
        with os.fdopen(descriptor, 'rb', closefd=False) as stream:
            data = stream.read(limit + 1)
        after = os.fstat(descriptor)
        require(len(data) <= limit and (before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'input changed or exceeds size bound')
        return data
    finally:
        os.close(descriptor)


def strict_json(data):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    def invalid_constant(_):
        raise ValueError('nonfinite JSON value')
    return json.loads(data, object_pairs_hook=pairs, parse_constant=invalid_constant)


def file_hash(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_size <= limit, 'bounded regular input required')
        with os.fdopen(descriptor, 'rb', closefd=False) as stream:
            digest = hashlib.sha256()
            count = 0
            while chunk := stream.read(1024 * 1024):
                count += len(chunk)
                require(count <= limit, 'input exceeds size bound')
                digest.update(chunk)
        after = os.fstat(descriptor)
        require((info.st_size, info.st_mtime_ns, info.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'input changed while reading')
        return digest.hexdigest()
    finally:
        os.close(descriptor)


def manifest_identity(raw, config_raw, reference, architecture):
    require(len(raw) <= 4 * 1024 * 1024 and len(config_raw) <= 4 * 1024 * 1024, 'metadata too large')
    require(reference.rsplit('@', 1)[-1] == 'sha256:' + sha(raw), 'registry manifest digest mismatch')
    manifest, config = strict_json(raw), strict_json(config_raw)
    require(manifest.get('schemaVersion') == 2 and 'manifests' not in manifest and
            manifest.get('mediaType') in ('application/vnd.oci.image.manifest.v1+json',
                                         'application/vnd.docker.distribution.manifest.v2+json'),
            'single native image manifest required')
    config_digest = 'sha256:' + sha(config_raw)
    require(manifest.get('config', {}).get('digest') == config_digest, 'registry config digest mismatch')
    require(config.get('architecture') == ARCHES[architecture][0] and config.get('os') == 'linux',
            'native Linux image required')
    require(isinstance(manifest.get('layers'), list) and manifest['layers'], 'completed image layers required')
    return config_digest


def command(*arguments):
    try:
        return subprocess.check_output(arguments, stderr=subprocess.PIPE)
    except subprocess.CalledProcessError:
        raise ValueError('external verification command failed') from None


def native_host(architecture):
    require(platform.system() == 'Linux' and platform.machine() == architecture and os.getuid() == 0,
            'already-root native Linux required')
    require(not any(os.environ.get(name) for name in
                    ('CONTAINER_HOST', 'CONTAINER_CONNECTION', 'CONTAINER_SSHKEY', 'DOCKER_HOST')),
            'remote container engines forbidden')


def inspect_local(reference, architecture, expected_id=None):
    images = strict_json(command('podman', '--remote=false', 'image', 'inspect', reference))
    require(isinstance(images, list) and len(images) == 1, 'one local image required')
    image = images[0]
    identity = 'sha256:' + image.get('Id', '').removeprefix('sha256:')
    require(bool(re.fullmatch(DIGEST, identity)) and image.get('Architecture') == ARCHES[architecture][0]
            and image.get('Os') == 'linux', 'local native image identity mismatch')
    if expected_id is not None:
        require(identity == expected_id, 'local config identity differs')
    if '@' in reference:
        require(reference in image.get('RepoDigests', []), 'pulled registry identity differs')
    return identity


@contextmanager
def mounted(identity):
    raw = command('podman', '--remote=false', 'image', 'mount', identity)
    try:
        path = Path(raw.decode().strip())
        require(path.is_absolute() and path.is_dir(), 'invalid image mount')
        yield path
    except BaseException as error:
        try:
            command('podman', '--remote=false', 'image', 'unmount', identity)
        except BaseException:
            # Keep the original failure (including cancellation), but never
            # silently lose the fact that an image may still be mounted.
            error.add_note('image unmount failed during error cleanup')
        raise
    else:
        command('podman', '--remote=false', 'image', 'unmount', identity)


def registry_identity(reference, architecture):
    return manifest_identity(command('skopeo', 'inspect', '--tls-verify=true', '--raw', 'docker://' + reference),
                             command('skopeo', 'inspect', '--tls-verify=true', '--config', '--raw', 'docker://' + reference),
                             reference, architecture)
