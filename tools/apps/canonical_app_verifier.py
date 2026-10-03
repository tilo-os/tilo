#!/usr/bin/env python3
"""Receipt adapter for canonical Rust CLIs; does not implement cryptography."""
import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib


def require(condition, message):
    if not condition:
        raise ValueError(message)


def file_sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


class CanonicalVerifier:
    def __init__(self, pkg_binary, runtime_binary, policy):
        self.pkg = Path(pkg_binary).resolve()
        self.runtime = Path(runtime_binary).resolve()
        self.keys = policy['app_publishers']
        self.runtime_key = policy['runtime_publisher']
        require(self.keys and all(re.fullmatch('[0-9a-f]{64}', key) for key in self.keys), 'explicit app trust keys required')
        require(re.fullmatch('[0-9a-f]{64}', self.runtime_key), 'explicit runtime trust key required')
        self.binary_hashes = {'tilo_pkg_sha256': file_sha(self.pkg), 'tilo_runtime_bundle_sha256': file_sha(self.runtime)}
        require(self.binary_hashes == policy['canonical_binaries'], 'canonical verifier binary digest mismatch')

    def run(self, binary, *args):
        expected = self.binary_hashes['tilo_pkg_sha256' if binary == self.pkg else 'tilo_runtime_bundle_sha256']
        require(file_sha(binary) == expected, 'canonical verifier binary changed')
        completed = subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True, timeout=120,
                                   env={'PATH': os.defpath, 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8'})
        require(completed.returncode == 0, f'canonical verification failed ({binary.name}, exit {completed.returncode}): {completed.stderr.strip()}')
        return completed.stdout

    def verify_tapp(self, path, app):
        before = file_sha(path)
        # Canonical read_tapp loads named members into its blob store. Reject
        # duplicates/special members before reading metadata independently so the
        # adapter and canonical verifier can never select different tar entries.
        with tarfile.open(path, 'r:') as archive:
            members = archive.getmembers()
            names = [member.name for member in members]
            require(len(names) <= 4096 and len(names) == len(set(names)), 'duplicate/excessive tapp members')
            for member in members:
                name = member.name
                require(member.isfile() and not member.issparse() and not PurePosixPath(name).is_absolute(), 'nonregular tapp member')
                require(name in {'tapp.toml', 'signed-root', 'manifest.toml', 'collection.qcl'}
                        or re.fullmatch('blobs/[0-9a-f]{64}', name), 'unexpected tapp member')
            manifest = tomllib.loads(archive.extractfile('manifest.toml').read().decode())
        flags = [argument for key in self.keys for argument in ('--trusted-key', key)]
        output = self.run(self.pkg, 'verify', *flags, path)
        # A release receipt covers the whole package, so only the full form (every
        # payload blob inside the archive) is accepted, never a thin one.
        verified = re.fullmatch(r'ok: (tapp:[0-9a-f]{64}) verifies and publisher is trusted \(full\)\s*', output)
        require(verified is not None, 'unexpected canonical package verification report')
        inspected = self.run(self.pkg, 'inspect', path).split('\n--- manifest.toml ---\n', 1)[0]
        package_ids = re.findall(r'^package-id: (tapp:[0-9a-f]{64})$', inspected, re.MULTILINE)
        publishers = re.findall(r'^publisher-identity: ed25519:([0-9a-f]{64})$', inspected, re.MULTILINE)
        require(package_ids == [verified.group(1)] and len(publishers) == 1 and publishers[0] in self.keys,
                'canonical package identity report mismatch')
        require(re.findall(r'^signed: (.+)$', inspected, re.MULTILINE) == ['yes'], 'canonical package is unsigned')
        require(manifest['package']['id'] == app['id'] and manifest['package']['version'] == app['version']
                and manifest['package']['runtime'] == app['runtime'], 'signed manifest does not match collection index')
        require(file_sha(path) == before, 'tapp changed during canonical verification')
        return {'package_id': verified.group(1), 'publisher': publishers[0]}

    def verify_runtime(self, directory, architecture):
        bundles = list(directory.glob('tilo-runtime/*/bundle.toml'))
        require(len(bundles) == 1, 'exactly one runtime bundle required')
        path = bundles[0]
        before = file_sha(path)
        self.run(self.runtime, 'verify', path.parent, '--require-publisher', self.runtime_key,
                 '--require-signature', '--require-arch', architecture)
        manifest = tomllib.loads(path.read_text())
        require(file_sha(path) == before, 'runtime manifest changed during canonical verification')
        require(manifest['version'] == path.parent.name and manifest['arch'] == architecture
                and manifest['publisher_hex'] == self.runtime_key, 'runtime identity mismatch')
        require((directory / 'runtime-bundle-publisher.hex').read_text().strip() == self.runtime_key, 'runtime publisher file mismatch')
        return {'version': manifest['version'], 'root_hash': manifest['root_hash'], 'publisher': manifest['publisher_hex']}
