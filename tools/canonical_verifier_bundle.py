#!/usr/bin/env python3
"""Create or materialize a platform-pinned, reusable native verifier bundle."""
import argparse
import gzip
import hashlib
import io
import json
import platform
from pathlib import Path
import re
import tarfile
import tempfile
import urllib.request

import app_channel_manifest as channel

NAMES = ('tilo-app-package', 'tilo-runtime-bundle')
KEYS = ('tilo_pkg_sha256', 'tilo_runtime_bundle_sha256')
MAX_BYTES = 128 * 1024**2


def native_arch():
    return {'AMD64': 'x86_64', 'ARM64': 'aarch64'}.get(platform.machine(), platform.machine())


def elf_arch(path, architecture):
    with path.open('rb') as stream:
        header = stream.read(20)
    channel.require(len(header) == 20 and header[:6] == b'\x7fELF\x02\x01'
                    and int.from_bytes(header[18:20], 'little') == {'x86_64': 62, 'aarch64': 183}[architecture],
                    'verifier must be a native ELF64 little-endian executable')


def identity(builder, source, architecture):
    channel.require(architecture in channel.ARCHES, 'unsupported verifier architecture')
    channel.digest(source, 40)
    channel.require(re.fullmatch(r'registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}', builder), 'immutable compatible builder required')


def produce(pkg, runtime, output, builder, source, architecture):
    identity(builder, source, architecture)
    binaries = dict(zip(NAMES, (pkg, runtime)))
    for binary in binaries.values():
        channel.require(binary.is_file() and not binary.is_symlink() and binary.stat().st_size <= MAX_BYTES, 'invalid verifier binary')
        elf_arch(binary, architecture)
    channel.require(sum(p.stat().st_size for p in binaries.values()) + 16384 <= MAX_BYTES, 'bundle executable set exceeds limit')
    hashes = {key: channel.sha(path) for key, path in zip(KEYS, (pkg, runtime))}
    manifest = {'schema': 'tilo.canonical-verifier-bundle/v1', 'architecture': architecture,
                'compatible_builder': builder, 'source_sha': source, 'canonical_binaries': hashes,
                'package_identity': 'blake3-ed25519-signed-root-v1', 'runtime_identity': 'blake3-ed25519-runtime-bundle-v1'}
    adapter = (Path(__file__).parent / 'apps/canonical_app_verifier.py').read_bytes()
    manifest['verifier_sha256'] = hashlib.sha256(adapter).hexdigest()
    raw = (json.dumps(manifest, sort_keys=True, separators=(',', ':')) + '\n').encode()
    with output.open('xb') as destination, gzip.GzipFile(filename='', mode='wb', fileobj=destination, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode='w') as archive:
            for name, data in [('manifest.json', raw), ('canonical_app_verifier.py', adapter)] + [(name, path.read_bytes()) for name, path in binaries.items()]:
                info = tarfile.TarInfo(name)
                info.mode = 0o644 if name == 'manifest.json' else 0o755
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
    channel.require(output.stat().st_size <= MAX_BYTES, 'bundle exceeds transport limit')
    sha = channel.sha(output)
    descriptor = {'sha256': sha, 'bytes': output.stat().st_size, 'canonical_binaries': hashes,
                  'source_sha': source, 'compatible_builder': builder, 'verifier_sha256': manifest['verifier_sha256'],
                  'url': f'https://code.tilo.so/api/packages/tilo/generic/tilo-canonical-verifiers/{sha}/{architecture}.tar.gz'}
    output.with_suffix('.descriptor.json').write_text(json.dumps(descriptor, sort_keys=True, indent=2) + '\n')
    return descriptor


def resolve_policy(policy, architecture):
    descriptor = policy['canonical_verifier_bundles'][architecture]
    channel.digest(descriptor['sha256'])
    channel.require(set(descriptor['canonical_binaries']) == set(KEYS), 'bundle binary identities required')
    for value in descriptor['canonical_binaries'].values():
        channel.digest(value)
    channel.require(policy['verifier_sha256'] == channel.digest(descriptor['verifier_sha256']), 'bundle adapter not approved')
    return dict(policy, canonical_binaries=descriptor['canonical_binaries'],
                canonical_bundle={'architecture': architecture, 'sha256': descriptor['sha256']})


def materialize(archive, descriptor, output, builder, architecture):
    identity(builder, descriptor['source_sha'], architecture)
    channel.require(builder == descriptor['compatible_builder'], 'verifier builder compatibility mismatch')
    channel.require(type(descriptor['bytes']) is int and 0 < descriptor['bytes'] <= MAX_BYTES
                    and archive.stat().st_size == descriptor['bytes'] and channel.sha(archive) == descriptor['sha256'], 'bundle digest/size mismatch')
    channel.require(not output.exists(), 'verifier destination must be fresh')
    # Bound decompression before tar metadata (including extended headers) is parsed.
    with gzip.open(archive, 'rb') as compressed:
        expanded = compressed.read(MAX_BYTES + 65537)
    channel.require(len(expanded) <= MAX_BYTES + 65536, 'expanded bundle exceeds limit')
    with tarfile.open(fileobj=io.BytesIO(expanded), mode='r:') as stream:
        members = []
        for member in stream:
            members.append(member)
            channel.require(len(members) <= 4, 'unsafe bundle inventory')
        channel.require(len(members) == 4 and {m.name for m in members} == {'manifest.json', 'canonical_app_verifier.py', *NAMES}
                        and all(m.isfile() and not m.issparse() and 0 <= m.size <= MAX_BYTES for m in members)
                        and sum(m.size for m in members) <= MAX_BYTES, 'unsafe bundle inventory')
        manifest_member = stream.getmember('manifest.json')
        channel.require(manifest_member.size <= 16384, 'oversized bundle manifest')
        manifest = channel.read_json(stream.extractfile(manifest_member).read())
        expected = {'schema': 'tilo.canonical-verifier-bundle/v1', 'architecture': architecture,
                    'compatible_builder': builder, 'source_sha': descriptor['source_sha'],
                    'canonical_binaries': descriptor['canonical_binaries'], 'verifier_sha256': descriptor['verifier_sha256'],
                    'package_identity': 'blake3-ed25519-signed-root-v1', 'runtime_identity': 'blake3-ed25519-runtime-bundle-v1'}
        channel.require(manifest == expected, 'bundle compatibility/identity mismatch')
        adapter = stream.extractfile('canonical_app_verifier.py').read()
        channel.require(hashlib.sha256(adapter).hexdigest() == descriptor['verifier_sha256'], 'bundle adapter digest mismatch')
        output.mkdir(parents=True)
        (output / 'canonical_app_verifier.py').write_bytes(adapter)
        for name, key in zip(NAMES, KEYS):
            data = stream.extractfile(name).read()
            channel.require(hashlib.sha256(data).hexdigest() == descriptor['canonical_binaries'][key], 'bundle executable digest mismatch')
            path = output / name
            path.write_bytes(data)
            elf_arch(path, architecture)
            path.chmod(0o755)
    return output


def fetch(policy_path, policy_sha, output, builder, token, architecture=None):
    architecture = architecture or native_arch()
    policy = channel.pinned_json(policy_path.read_bytes(), policy_sha)
    resolved = resolve_policy(policy, architecture)
    descriptor = policy['canonical_verifier_bundles'][architecture]
    expected = f"https://code.tilo.so/api/packages/tilo/generic/tilo-canonical-verifiers/{descriptor['sha256']}/{architecture}.tar.gz"
    channel.require(descriptor['url'] == expected, 'bundle URL must pin its content digest at the trusted origin')
    channel.require(descriptor['compatible_builder'] == builder, 'verifier builder compatibility mismatch')
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    request = urllib.request.Request(expected, headers={'Authorization': 'Bearer ' + token} if token else {})
    channel.require(type(descriptor['bytes']) is int and 0 < descriptor['bytes'] <= MAX_BYTES, 'invalid bundle size')
    # Own the scratch archive, not a predictable sibling of the destination.
    # Failed or interrupted downloads must not block the next invocation.
    with tempfile.TemporaryDirectory(prefix='tilo-verifier-download-') as temporary:
        archive = Path(temporary) / 'bundle.tar.gz'
        with urllib.request.build_opener(NoRedirect).open(request, timeout=60) as response, archive.open('xb') as destination:
            size = 0
            while data := response.read(1024**2):
                size += len(data)
                channel.require(size <= descriptor['bytes'], 'oversized bundle download')
                destination.write(data)
        materialize(archive, descriptor, output, builder, architecture)
    return resolved


def main():
    import os
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    producer = commands.add_parser('produce')
    for name in ('pkg-binary', 'runtime-binary', 'output'):
        producer.add_argument('--' + name, type=Path, required=True)
    for name in ('builder-image', 'source-sha', 'architecture'):
        producer.add_argument('--' + name, required=True)
    consumer = commands.add_parser('fetch')
    consumer.add_argument('--trust-policy', type=Path, required=True)
    consumer.add_argument('--trust-policy-sha256', required=True)
    consumer.add_argument('--output', type=Path, required=True)
    consumer.add_argument('--builder-image', required=True)
    args = parser.parse_args()
    if args.command == 'produce':
        produce(args.pkg_binary, args.runtime_binary, args.output, args.builder_image, args.source_sha, args.architecture)
    else:
        fetch(args.trust_policy, args.trust_policy_sha256, args.output, args.builder_image,
              os.environ.get('VERIFIER_BUNDLE_READ_TOKEN', ''))


if __name__ == '__main__':
    main()
